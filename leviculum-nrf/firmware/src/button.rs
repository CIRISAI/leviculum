//! The user button's press handling (Codeberg #459, the gestures of
//! `docs/src/concepts/one-button.md`; moved here in #469 976).
//!
//! What counts as a press, which gesture it is and how often one may
//! announce is decided in [`leviculum_announce_policy::ButtonGate`]; this
//! module times the edges it reads from a [`ButtonInput`] against a
//! [`Clock`], writes the `BTN_PRESS` and `BTN_HOLD` lines, raises
//! [`ANNOUNCE_PRESS`] for a double press, wakes the display where there is
//! one and asks [`crate::feedback`] for the status and "release now"
//! patterns. The announces themselves go out from the node loop, which owns
//! the node and waits on [`announce_press`]. The pin, its pull and which
//! level a press drives stay with the bind crate
//! (`leviculum_nrf::button::NrfButton`).
//!
//! | Gesture | `action=` | Does |
//! |---|---|---|
//! | short | `status` | wakes the display (Pocket V2), then the battery in quarters on the LED and the buzzer ([`Feedback::status`]) |
//! | double | `announce` / `rate-limited` | both announces, at most one set per 30 s, then one position report if a recipient is set (the node loop, on [`ANNOUNCE_PRESS`]) |
//! | long, 2 s | `long` | nothing yet: the mode switch, once Tracker mode exists; the 2 s mark blips "release now" |
//! | very long, 8 s | `hold8` | the board switches off ([`POWER_OFF`], order 1511); the 8 s mark plays `power-off` |
//!
//! # The Pocket V2's display
//!
//! Any press lights a dark display, and the display goes dark on its own
//! a minute after the last press; no press turns it off
//! (`leviculum_screen::power::DisplayWake`). The press handling raises
//! `board_state::DISPLAY_WAKE` on every press; `crate::display` owns the
//! state and the timeout. The T114's ST7789 has no power
//! control yet and stays lit, as before.
//!
//! # One registered instance
//!
//! The handling is [`Button`], over a [`Feedback`] and the two signals it
//! raises, so a host test holds one over `fakes::RecordingSinks` and
//! signals of its own. The board's one instance is the one [`run`] builds,
//! over the registered feedback sinks (`feedback::set_sinks`) and the
//! statics; it goes when step 7's node loop holds its own.

use core::future::pending;

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use leviculum_announce_policy::{
    ButtonEvent, ButtonGate, PressAction, HOLD8_MIN_MS, LONG_PRESS_MIN_MS,
};

use crate::feedback::{Feedback, Sinks};
use crate::seams::{ButtonInput, Clock};

/// Raised once per double press that announces; the node loop waits on it beside
/// its other inputs. A `Signal` rather than a channel because the rate
/// limit already ran: two raises the loop has not consumed yet would be
/// two presses inside one window, which the gate never lets through.
pub static ANNOUNCE_PRESS: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Wait for a press that announces. For the node loop's select.
pub async fn announce_press() {
    ANNOUNCE_PRESS.wait().await;
}

/// Raised once by a very long press at its release (order 1511): the bind
/// crate's button task waits on it beside the press handling and switches
/// the board off. The signal goes up at the release, not at the 8 s mark,
/// because the button is the wake source: a board switched off with the
/// button still down would sense the press it is waiting for and wake at
/// once.
pub static POWER_OFF: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// The board's button, until it is released: the press handling over the
/// registered feedback sinks, [`ANNOUNCE_PRESS`] and, on a board with the
/// `display` capability, `board_state::DISPLAY_WAKE`. `board` is the
/// `board=` token of the `BTN_PRESS` and `BTN_HOLD` lines.
pub async fn run<I: ButtonInput, C: Clock>(mut input: I, clock: C, board: &str) -> ! {
    #[cfg(feature = "display")]
    let wake = Some(&crate::board_state::DISPLAY_WAKE);
    #[cfg(not(feature = "display"))]
    let wake = None;
    Button::new(
        board,
        &crate::feedback::FEEDBACK,
        crate::feedback::battery_percent,
        &ANNOUNCE_PRESS,
        &POWER_OFF,
        wake,
    )
    .run(&mut input, &clock)
    .await
}

/// One line the press handling writes, as its body: the formatter appends
/// the `t=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line<'a> {
    /// A held button reached `ms`.
    Hold { board: &'a str, ms: u64 },
    /// A finished gesture, or a glitch.
    Press {
        board: &'a str,
        action: PressAction,
        held_ms: u64,
    },
}

impl core::fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Line::Hold { board, ms } => write!(f, "BTN_HOLD board={} ms={}", board, ms),
            Line::Press {
                board,
                action,
                held_ms,
            } => write!(
                f,
                "BTN_PRESS board={} action={} held_ms={}",
                board,
                action.as_str(),
                held_ms
            ),
        }
    }
}

/// The press handling: the gate, and what each event it reports does.
pub struct Button<'a, S> {
    board: &'a str,
    gate: ButtonGate,
    feedback: &'a Feedback<S>,
    battery: fn() -> Option<u8>,
    announce: &'a Signal<CriticalSectionRawMutex, ()>,
    power_off: &'a Signal<CriticalSectionRawMutex, ()>,
    wake: Option<&'a Signal<CriticalSectionRawMutex, ()>>,
    /// The lines written, for a test to read.
    #[cfg(test)]
    written: alloc::vec::Vec<alloc::string::String>,
}

impl<'a, S: Sinks> Button<'a, S> {
    /// A released button. `battery` reads the percentage a short press
    /// reports; `wake` is the display's, `None` on a board without one.
    pub fn new(
        board: &'a str,
        feedback: &'a Feedback<S>,
        battery: fn() -> Option<u8>,
        announce: &'a Signal<CriticalSectionRawMutex, ()>,
        power_off: &'a Signal<CriticalSectionRawMutex, ()>,
        wake: Option<&'a Signal<CriticalSectionRawMutex, ()>>,
    ) -> Self {
        Self {
            board,
            gate: ButtonGate::new(),
            feedback,
            battery,
            announce,
            power_off,
            wake,
            #[cfg(test)]
            written: alloc::vec::Vec::new(),
        }
    }

    /// Read `input` against `clock`, forever.
    pub async fn run<I: ButtonInput, C: Clock>(&mut self, input: &mut I, clock: &C) -> ! {
        // The level the gate last heard about. An edge wait may sense the
        // opposite of the level at the moment it arms (the GPIOTE does),
        // so a change between the last read and the arming would only wake
        // us on its way back; reading again right before arming, with no
        // await between, hands that change to the gate as the edge it is.
        let mut seen = false;
        loop {
            let level = input.is_pressed();
            if level != seen {
                self.gate.edge(clock.now_ms(), level);
            }
            let deadline = self.gate.deadline_ms();
            let quiet = async {
                match deadline {
                    Some(at) => clock.wait_until(at).await,
                    None => pending::<()>().await,
                }
            };
            let woke = select(input.wait_for_edge(), quiet).await;
            let now_ms = clock.now_ms();
            let pressed = input.is_pressed();
            seen = pressed;
            match woke {
                Either::First(()) => self.gate.edge(now_ms, pressed),
                Either::Second(()) => {
                    if let Some(event) = self.gate.settle(now_ms, pressed) {
                        self.on_event(event);
                    }
                }
            }
        }
    }

    fn write(&mut self, line: Line) {
        crate::log::log_fmt_critical("", format_args!("{}", line));
        #[cfg(test)]
        self.written.push(alloc::format!("{}", line));
    }

    fn on_event(&mut self, event: ButtonEvent) {
        let press = match event {
            ButtonEvent::Press(press) => press,
            // The moment a held button reaches 2 s and 8 s, so the rig can
            // see it. At 2 s the board blips "release now" (order 892), at
            // 8 s it plays `power-off`, the light running down (order 1511).
            // The gesture itself is the `BTN_PRESS` line at the release.
            ButtonEvent::Held { ms } => {
                self.write(Line::Hold {
                    board: self.board,
                    ms,
                });
                if ms == LONG_PRESS_MIN_MS {
                    self.feedback.release_now();
                } else if ms == HOLD8_MIN_MS {
                    self.feedback.power_off();
                }
                return;
            }
        };
        self.write(Line::Press {
            board: self.board,
            action: press.action,
            held_ms: press.held_ms,
        });
        // Any press lights a dark display and restarts its minute; a
        // debounced glitch was never a press.
        if press.action != PressAction::Debounced {
            if let Some(wake) = self.wake {
                wake.signal(());
            }
        }
        match press.action {
            // A double press outside the 30 s window: both announces.
            PressAction::Announce => self.announce.signal(()),
            // A short press is status: after the display wake above, the
            // battery in quarters on the LED and the buzzer (order 892).
            // The mode blink joins it once there is a mode.
            PressAction::Status => self.feedback.status((self.battery)()),
            // The very long press switches the board off, at the release
            // (`POWER_OFF` says why not at the mark).
            PressAction::Hold8 => self.power_off.signal(()),
            // Recognised and reported, nothing else: the long press becomes
            // the mode switch in the pass that builds Tracker mode.
            PressAction::Long => {}
            PressAction::RateLimited | PressAction::Debounced => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{drive, FakeButtonInput, FakeClock, RecordingSinks};
    use crate::feedback::Sink;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    const LED_AND_BUZZER: &[Sink] = &[Sink::Led, Sink::Buzzer];

    /// What one scripted run left behind.
    struct Outcome {
        lines: Vec<String>,
        played: Vec<(Sink, &'static str)>,
        announces: usize,
        wakes: usize,
        power_offs: usize,
    }

    /// Run the press handling over `edges` (`(t_ms, pressed)`) until the
    /// script is spent and no deadline is pending, or `until_ms`
    /// (`fakes::drive`). Before every poll the two signals are taken, so a
    /// count of two is two raises, not one raise seen twice.
    fn press(edges: &[(u64, bool)], until_ms: u64) -> Outcome {
        let feedback = Feedback::new(RecordingSinks::new(LED_AND_BUZZER));
        let announce = Signal::new();
        let power_off = Signal::new();
        let wake = Signal::new();
        let clock = FakeClock::new();
        let input = FakeButtonInput::new(&clock, edges);
        let mut button = Button::new(
            "test",
            &feedback,
            || Some(60),
            &announce,
            &power_off,
            Some(&wake),
        );
        let (mut announces, mut wakes, mut power_offs) = (0, 0, 0);
        let mut handle = &input;
        drive(button.run(&mut handle, &clock), &clock, until_ms, |_| {
            announces += usize::from(announce.try_take().is_some());
            wakes += usize::from(wake.try_take().is_some());
            power_offs += usize::from(power_off.try_take().is_some());
            input.next_edge_ms()
        });
        Outcome {
            lines: button.written.clone(),
            played: feedback
                .sinks()
                .calls()
                .into_iter()
                .map(|(_, sink, name)| (sink, name))
                .collect(),
            announces,
            wakes,
            power_offs,
        }
    }

    // A press at 1000 released at 1100: the gate settles it down at 1050
    // (`Debouncer::edge` sets `quiet_until_ms = now + BUTTON_DEBOUNCE_MS`),
    // up at 1150 with `held_ms = burst_start - pressed_at = 100`, and opens
    // the window at `pressed_at + held_ms = 1100`; it closes at 1500 with no
    // second press (`window_deadline_ms`), `close_window` reports `Status`.
    // `on_event`: the line, the wake (`action != Debounced`), and
    // `feedback.status(battery())`, which plays the quarters of 60 % on the
    // LED and the buzzer (`Feedback::status`).
    #[test]
    fn a_short_press_is_status_wakes_the_display_and_plays_the_battery() {
        let out = press(&[(1_000, true), (1_100, false)], 60_000);
        assert_eq!(
            out.lines,
            vec!["BTN_PRESS board=test action=status held_ms=100"]
        );
        let quarters = leviculum_feedback::battery_quarters(60).name;
        assert_eq!(
            out.played,
            vec![(Sink::Led, quarters), (Sink::Buzzer, quarters)]
        );
        assert_eq!(out.wakes, 1);
        assert_eq!(out.announces, 0);
    }

    // Two presses of 100 ms, the second starting 100 ms after the first's
    // release: it settles down at 1250 with `pressed_at = 1200 <= 1500`
    // (`down`, `SecondDown`), its release settles at 1350, and `up`
    // asks `PressLimiter::press(1350)`, the first: `Announce`, held 100.
    // The same double at 10 s is inside `last + 30 s`: `RateLimited`, no
    // second signal. Both wake the display; neither plays a pattern.
    #[test]
    fn a_double_press_announces_once_and_a_second_inside_30_s_is_rate_limited() {
        let out = press(
            &[
                (1_000, true),
                (1_100, false),
                (1_200, true),
                (1_300, false),
                (10_000, true),
                (10_100, false),
                (10_200, true),
                (10_300, false),
            ],
            60_000,
        );
        assert_eq!(
            out.lines,
            vec![
                "BTN_PRESS board=test action=announce held_ms=100",
                "BTN_PRESS board=test action=rate-limited held_ms=100",
            ]
        );
        assert_eq!(out.announces, 1);
        assert_eq!(out.wakes, 2);
        assert_eq!(out.played, vec![]);
    }

    // Held from 1000 to 3500: settled down at 1050, the hold deadline
    // `pressed_at + LONG_PRESS_MIN_MS = 3000` reports `Held { ms: 2000 }`,
    // which writes `BTN_HOLD` and, being the long mark, one `release_now`
    // (`if ms == LONG_PRESS_MIN_MS`). The release settles at 3550 with
    // `held_ms = 2500 >= LONG_PRESS_MIN_MS`: `Long`, which only wakes.
    #[test]
    fn a_hold_to_2_s_writes_btn_hold_and_blips_release_now_once() {
        let out = press(&[(1_000, true), (3_500, false)], 60_000);
        assert_eq!(
            out.lines,
            vec![
                "BTN_HOLD board=test ms=2000",
                "BTN_PRESS board=test action=long held_ms=2500",
            ]
        );
        assert_eq!(
            out.played,
            vec![(Sink::Led, "release-now"), (Sink::Buzzer, "release-now")]
        );
        assert_eq!(out.wakes, 1);
        assert_eq!(out.announces, 0);
    }

    // Held from 1000 to 9500: the 2 s mark as above, then the second hold
    // deadline `pressed_at + HOLD8_MIN_MS = 9000` reports `Held { ms: 8000 }`,
    // which writes `BTN_HOLD` and plays `power-off` (`else if ms ==
    // HOLD8_MIN_MS`). The release settles at 9550, `held_ms = 8500 >=
    // HOLD8_MIN_MS`: `Hold8`, which wakes the display and raises
    // `power_off` once, at the release and not at the mark.
    #[test]
    fn a_hold_to_8_s_plays_power_off_at_the_mark_and_switches_off_at_the_release() {
        let out = press(&[(1_000, true), (9_500, false)], 60_000);
        assert_eq!(
            out.lines,
            vec![
                "BTN_HOLD board=test ms=2000",
                "BTN_HOLD board=test ms=8000",
                "BTN_PRESS board=test action=hold8 held_ms=8500",
            ]
        );
        assert_eq!(
            out.played,
            vec![
                (Sink::Led, "release-now"),
                (Sink::Buzzer, "release-now"),
                (Sink::Led, "power-off"),
                (Sink::Buzzer, "power-off"),
            ]
        );
        assert_eq!(out.power_offs, 1);
        assert_eq!(out.announces, 0);
    }

    // The same press still held: the mark has played, the board stays on.
    #[test]
    fn a_button_still_held_past_8_s_does_not_switch_off_yet() {
        let out = press(&[(1_000, true)], 20_000);
        assert_eq!(out.power_offs, 0);
        assert!(out.played.contains(&(Sink::Led, "power-off")));
    }

    // Down at 1000, up at 1030: the second edge restarts the quiet period
    // to 1080, where the level is back at the accepted one, released:
    // `Settled::Bounce`, reported as `Debounced` with `held_ms` 0. No wake
    // (`action != Debounced`), nothing played, nothing announced.
    #[test]
    fn a_30_ms_glitch_is_debounced_and_does_nothing() {
        let out = press(&[(1_000, true), (1_030, false)], 60_000);
        assert_eq!(
            out.lines,
            vec!["BTN_PRESS board=test action=debounced held_ms=0"]
        );
        assert_eq!(out.wakes, 0);
        assert_eq!(out.played, vec![]);
        assert_eq!(out.announces, 0);
    }
}
