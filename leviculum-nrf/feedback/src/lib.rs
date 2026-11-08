//! The one-button feedback grammar as on/off patterns (Codeberg #459,
//! order 892; the grammar is `docs/src/concepts/one-button.md`).
//!
//! | signal | pattern | constructor |
//! |---|---|---|
//! | three short | Tracker | [`mode`]`(Mode::Tracker)` |
//! | one long | Mesh | [`mode`]`(Mode::Mesh)` |
//! | *n* short | battery, in quarters | [`battery_quarters`] |
//! | fast flicker | error | [`error`] |
//! | one short | a message for the user arrived | [`notify`] |
//! | one blip | a held button has reached 2 s: release now | [`release_now`] |
//! | two short | the report a double press sent was proven delivered | [`report_delivered`] |
//! | one short, one blip | the announces a double press made have left the radio | [`announce_sent`] |
//! | one long, one short, one blip | a held button has reached 8 s: the board switches off at the release | [`power_off`] |
//! | fast flicker, three seconds | the button held through the boot is about to wipe the board: let go to keep it | [`reset_warning`] |
//!
//! A [`Pattern`] is a short list of [`Step`]s, each an on time and the
//! off time after it, and it is the same list for every sink: the LED, the
//! notification LED and the T1000-E's buzzer play one pattern the same way,
//! the buzzer by sounding its tone where the LED lights. The firmware's
//! players (`leviculum-nrf/src/feedback.rs`) drive a pin from a
//! [`Playback`]; which phase a pattern is in at a given millisecond is
//! decided here, where it can be asserted without a board.
//!
//! When `announce-sent` plays is [`AirWatch`]'s decision, host-tested here
//! for the same reason.
//!
//! Every pattern is a `static` list, so a `Pattern` is two words and can
//! be handed to a player through a signal without a copy of its steps.

#![no_std]

/// One blink: the sink is on for `on_ms`, then off for `off_ms` before the
/// next step. The last step's `off_ms` is not played: a pattern ends when
/// its last on time ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// How long the sink is on.
    pub on_ms: u16,
    /// How long it stays off before the next step.
    pub off_ms: u16,
}

/// A named list of steps. The name is the `pattern=` token of the
/// firmware's `FEEDBACK` line, which is what the rig reads instead of the
/// light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    /// The `pattern=` token.
    pub name: &'static str,
    /// The steps, played in order.
    pub steps: &'static [Step],
}

impl Pattern {
    /// The time from the first on to the end of the last on: every step's
    /// on time and every off time but the last.
    pub fn total_ms(&self) -> u32 {
        let mut total: u32 = 0;
        for (i, step) in self.steps.iter().enumerate() {
            total += u32::from(step.on_ms);
            if i + 1 < self.steps.len() {
                total += u32::from(step.off_ms);
            }
        }
        total
    }
}

/// One counted blink is on this long. Long enough that an LED seen out of
/// the corner of an eye registers it as a deliberate blink rather than a
/// glitch (the activity pulse of `src/led.rs` is 80 ms and reads as a
/// flicker of traffic), short enough that four of them are over in under
/// two seconds.
pub const SHORT_ON_MS: u16 = 150;
/// The dark time between two counted blinks. More than twice the on time,
/// so the gaps, not the blinks, are what the eye counts, and a count of
/// four is two blinks a second: slow enough to count without effort.
pub const COUNT_GAP_MS: u16 = 350;
/// The Mesh mode's one long. Over six counted blinks long, so it cannot be
/// mistaken for a short however the eye catches it, and still only a
/// second.
pub const LONG_ON_MS: u16 = 1_000;
/// The error flicker's on and off time: 10 Hz. Five times the counting
/// cadence, so nobody tries to count it, and far below the ~50 Hz at which
/// an LED's blinking fuses into steady light.
pub const FLICKER_MS: u16 = 50;
/// How many flicker blinks the error pattern plays: two seconds, bounded,
/// so an error is a moment of flicker and never a state the LED is stuck
/// in.
pub const ERROR_FLASHES: usize = 20;
/// The "release now" blip at a held button's 2 s mark. Shorter than a
/// counted blink, so it is not read as one battery quarter, and about the
/// shortest beep Meshtastic plays as a click (`DURATION_1_16` = 62 ms,
/// `src/buzz/buzz.cpp`), which is audible on the T1000-E's buzzer.
pub const RELEASE_BLIP_MS: u16 = 60;
/// The dark time inside `announce-sent`, between its short and its blip.
/// Under the counting gap, so the pair reads as one figure, a short with a
/// click after it, and not as a count of two.
pub const FIGURE_GAP_MS: u16 = SHORT_ON_MS;
/// How many flicker blinks the factory reset's warning plays: three
/// seconds, half again the error's two, so the warning is the flicker the
/// user knows as "something is wrong" and long enough to let go during it
/// (order 1511).
pub const RESET_WARNING_FLASHES: usize = 30;

const SHORT: Step = Step {
    on_ms: SHORT_ON_MS,
    off_ms: COUNT_GAP_MS,
};
const FLICKER: Step = Step {
    on_ms: FLICKER_MS,
    off_ms: FLICKER_MS,
};

static SHORTS: [Step; 4] = [SHORT; 4];
static LONG: [Step; 1] = [Step {
    on_ms: LONG_ON_MS,
    off_ms: 0,
}];
static FLICKERS: [Step; ERROR_FLASHES] = [FLICKER; ERROR_FLASHES];
static BLIP: [Step; 1] = [Step {
    on_ms: RELEASE_BLIP_MS,
    off_ms: 0,
}];
static WARNING_FLICKERS: [Step; RESET_WARNING_FLASHES] = [FLICKER; RESET_WARNING_FLASHES];
static RUN_DOWN: [Step; 3] = [
    Step {
        on_ms: LONG_ON_MS,
        off_ms: FIGURE_GAP_MS,
    },
    Step {
        on_ms: SHORT_ON_MS,
        off_ms: FIGURE_GAP_MS,
    },
    Step {
        on_ms: RELEASE_BLIP_MS,
        off_ms: 0,
    },
];
static SHORT_BLIP: [Step; 2] = [
    Step {
        on_ms: SHORT_ON_MS,
        off_ms: FIGURE_GAP_MS,
    },
    Step {
        on_ms: RELEASE_BLIP_MS,
        off_ms: 0,
    },
];

/// The two operating modes (`docs/src/concepts/operating-modes.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Three short.
    Tracker,
    /// One long.
    Mesh,
}

/// The mode blink: three short for Tracker, one long for Mesh. Nothing
/// plays it yet, and order 1511 left it so on purpose: it waits for the
/// 2 s mode switch, which comes with the order that builds Tracker mode
/// (#466, after the PPK2 current baseline Lew asked for before any tracker
/// code; `docs/src/concepts/operating-modes.md`).
pub fn mode(mode: Mode) -> Pattern {
    match mode {
        Mode::Tracker => Pattern {
            name: "mode-tracker",
            steps: &SHORTS[..3],
        },
        Mode::Mesh => Pattern {
            name: "mode-mesh",
            steps: &LONG,
        },
    }
}

/// The battery in quarters: one short for 0 to 25 %, two for 26 to 50 %,
/// three for 51 to 75 %, four for 76 to 100 %. An empty pack still blinks
/// once, so a status press always answers; anything over 100 is four.
pub fn battery_quarters(pct: u8) -> Pattern {
    let quarters = usize::from(pct.min(100)).div_ceil(25).max(1);
    const NAMES: [&str; 4] = ["battery-1", "battery-2", "battery-3", "battery-4"];
    Pattern {
        name: NAMES[quarters - 1],
        steps: &SHORTS[..quarters],
    }
}

/// What a short press plays: the battery in quarters, or nothing when the
/// board has no percentage. A board on USB without a pack, or a pack
/// outside the band its cell count implies, is not in error, so it stays
/// quiet rather than flicker on every press (order 893).
pub fn status(pct: Option<u8>) -> Option<Pattern> {
    pct.map(battery_quarters)
}

/// The error flicker: [`ERROR_FLASHES`] blinks at 10 Hz, then dark.
pub fn error() -> Pattern {
    Pattern {
        name: "error",
        steps: &FLICKERS,
    }
}

/// A message for the user arrived: one short.
pub fn notify() -> Pattern {
    Pattern {
        name: "notify",
        steps: &SHORTS[..1],
    }
}

/// The position report a double press sent has been proven delivered
/// (order 914): two short.
///
/// Two, because the answer echoes the question: the user pressed twice to
/// send, and two blinks say "what you sent arrived". One short is taken:
/// it is [`notify`], a message *for* the user, and the opposite direction
/// must not look the same. The shape is battery-2's, and the two do not
/// meet: the battery answers a single press within half a second of its
/// release, this one answers a double press after a proof round trip,
/// seconds later, and nothing else plays it. Silence is "not confirmed",
/// not "not sent": a report whose proof never comes plays nothing.
pub fn report_delivered() -> Pattern {
    Pattern {
        name: "report-delivered",
        steps: &SHORTS[..2],
    }
}

/// The announces a double press made have left the radio (order 930,
/// Codeberg #459): one short, then one blip.
///
/// Every double press gets this answer, with or without a report
/// recipient; before it, a board with no recipient answered the gesture
/// with nothing at all. It is neither of the one-step patterns it borrows
/// from: [`battery_quarters`]`(25)` and [`notify`] are one short and end
/// there, [`release_now`] is one blip and nothing before it. Against
/// [`report_delivered`], two short, it is the second step that differs:
/// a blip under half a short's length, after a gap under half the
/// counting one, so the figure is over in 360 ms where two short take
/// 650. The two follow each other on a board with a recipient, this one at
/// the air and that one seconds later at the proof.
pub fn announce_sent() -> Pattern {
    Pattern {
        name: "announce-sent",
        steps: &SHORT_BLIP,
    }
}

/// When a double press's announces have left the radio (order 930).
///
/// The LoRa interface numbers every packet it accepts into its queue
/// (the accept count after it, from 1) and the LoRa task settles each one
/// in that order: keyed, or dropped by the mute, the stale rule or a
/// refused frame. The press's announces are the packets accepted while
/// its arm dispatched them, so the last of them is the accept count after
/// the dispatch, and `announce-sent` plays when that packet settles keyed.
/// One watch per press, on the press's last packet, so a press plays it
/// once however many announces it made.
///
/// The LoRa task runs above the node loop, so a packet can settle between
/// the dispatch and the arm. The watch is therefore opened before the
/// dispatch ([`open`](Self::open)), remembers the latest settle while it
/// collects, and [`close`](Self::close) resolves a packet that has already
/// gone. A new press replaces a watch still waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirWatch {
    /// No press is waiting.
    Idle,
    /// A press is dispatching; the latest packet settled meanwhile.
    Collecting {
        /// The latest settle: the packet's number and whether it was keyed.
        last: Option<(u32, bool)>,
    },
    /// A press is waiting for the packet with this number.
    Armed {
        /// The press's last packet.
        packet: u32,
    },
}

impl Default for AirWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl AirWatch {
    /// No press waiting.
    pub const fn new() -> Self {
        Self::Idle
    }

    /// A press is about to dispatch its announces.
    pub fn open(&mut self) {
        *self = Self::Collecting { last: None };
    }

    /// The press's dispatch is done. `before` and `after` are the LoRa
    /// accept count around it; `announced` is whether the press made any
    /// announce (a press the duty budget defers makes none); `lora_active`
    /// is whether the LoRa carrier is on.
    ///
    /// No LoRa packet with LoRa on means the queue refused them, so the
    /// radio sends nothing and nothing plays. No LoRa packet with LoRa off
    /// means the announces went to the other carriers only, which have no
    /// air hook, and the dispatch itself is the answer.
    pub fn close(
        &mut self,
        before: u32,
        after: u32,
        announced: bool,
        lora_active: bool,
    ) -> Option<Pattern> {
        let last = match *self {
            Self::Collecting { last } => last,
            Self::Idle | Self::Armed { .. } => None,
        };
        *self = Self::Idle;
        if !announced {
            return None;
        }
        if after == before {
            return (!lora_active).then(announce_sent);
        }
        match last {
            Some((packet, keyed)) if packet == after => keyed.then(announce_sent),
            _ => {
                *self = Self::Armed { packet: after };
                None
            }
        }
    }

    /// The LoRa task settled the packet numbered `packet`; `keyed` is
    /// whether it went on the air.
    pub fn settled(&mut self, packet: u32, keyed: bool) -> Option<Pattern> {
        match *self {
            Self::Idle => None,
            Self::Collecting { .. } => {
                *self = Self::Collecting {
                    last: Some((packet, keyed)),
                };
                None
            }
            Self::Armed { packet: want } if want == packet => {
                *self = Self::Idle;
                keyed.then(announce_sent)
            }
            Self::Armed { .. } => None,
        }
    }
}

/// A held button has reached the long press: one blip, so the user knows
/// to let go.
pub fn release_now() -> Pattern {
    Pattern {
        name: "release-now",
        steps: &BLIP,
    }
}

/// A held button has reached 8 s, the very long press (order 1511): one
/// long, one short, one blip, the light running down. The board switches
/// off when the button is released.
///
/// Each blink shorter than the one before it, so the figure reads as a
/// light going out, and with the figure gap between them as one figure,
/// not as a count. None of its parts alone is new (the long is Mesh, the
/// short a battery quarter, the blip "release now"), and their sequence is
/// none of the others: a user who held through the 2 s blip sees a
/// different answer at 8 s.
pub fn power_off() -> Pattern {
    Pattern {
        name: "power-off",
        steps: &RUN_DOWN,
    }
}

/// The button has been held through the boot for the factory reset's
/// hold (order 1511): [`RESET_WARNING_FLASHES`] blinks at 10 Hz. The wipe
/// comes at its end if the button is still held; letting go during it
/// keeps everything.
pub fn reset_warning() -> Pattern {
    Pattern {
        name: "reset-warning",
        steps: &WARNING_FLICKERS,
    }
}

/// Where a pattern is at a given millisecond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The sink is on until `until_ms`.
    On {
        /// When this on time ends.
        until_ms: u64,
    },
    /// The sink is off until `until_ms`, between two steps.
    Off {
        /// When the next step starts.
        until_ms: u64,
    },
    /// The pattern has ended; the sink is off.
    Done,
}

/// A pattern started at a given millisecond. A new pattern replaces a
/// running one by starting a new `Playback`; nothing of the old one is
/// carried over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Playback {
    pattern: Pattern,
    start_ms: u64,
}

impl Playback {
    /// Start `pattern` at `start_ms`.
    pub fn new(pattern: Pattern, start_ms: u64) -> Self {
        Self { pattern, start_ms }
    }

    /// The pattern being played.
    pub fn pattern(&self) -> Pattern {
        self.pattern
    }

    /// The phase at `now_ms`. Before the start it is the first step's on
    /// time, so a player that reads the clock a little late still begins
    /// with the light on.
    pub fn phase(&self, now_ms: u64) -> Phase {
        let mut t = self.start_ms;
        let last = self.pattern.steps.len().saturating_sub(1);
        for (i, step) in self.pattern.steps.iter().enumerate() {
            let on_end = t + u64::from(step.on_ms);
            if now_ms < on_end {
                return Phase::On { until_ms: on_end };
            }
            if i == last {
                break;
            }
            let off_end = on_end + u64::from(step.off_ms);
            if now_ms < off_end {
                return Phase::Off { until_ms: off_end };
            }
            t = off_end;
        }
        Phase::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ons(p: &Pattern) -> [u16; ERROR_FLASHES] {
        let mut out = [0; ERROR_FLASHES];
        for (o, s) in out.iter_mut().zip(p.steps) {
            *o = s.on_ms;
        }
        out
    }

    #[test]
    fn tracker_is_three_short_and_mesh_one_long() {
        let tracker = mode(Mode::Tracker);
        assert_eq!(tracker.name, "mode-tracker");
        assert_eq!(tracker.steps, &[SHORT; 3]);
        assert_eq!(tracker.total_ms(), 3 * 150 + 2 * 350);
        let mesh = mode(Mode::Mesh);
        assert_eq!(mesh.name, "mode-mesh");
        assert_eq!(mesh.steps.len(), 1);
        assert_eq!(mesh.steps[0].on_ms, 1_000);
        assert_eq!(mesh.total_ms(), 1_000);
    }

    #[test]
    fn the_battery_is_counted_in_quarters_from_one_to_four() {
        let cases: [(u8, usize); 12] = [
            (0, 1),
            (1, 1),
            (25, 1),
            (26, 2),
            (50, 2),
            (51, 3),
            (75, 3),
            (76, 4),
            (99, 4),
            (100, 4),
            (101, 4),
            (255, 4),
        ];
        for (pct, n) in cases {
            let p = battery_quarters(pct);
            assert_eq!(p.steps.len(), n, "pct={pct}");
            assert!(p.steps.iter().all(|s| *s == SHORT), "pct={pct}");
            assert_eq!(p.name.as_bytes()[8] - b'0', n as u8, "pct={pct}");
        }
    }

    #[test]
    fn each_battery_count_has_its_total_length() {
        assert_eq!(battery_quarters(10).total_ms(), 150);
        assert_eq!(battery_quarters(40).total_ms(), 650);
        assert_eq!(battery_quarters(60).total_ms(), 1_150);
        assert_eq!(battery_quarters(90).total_ms(), 1_650);
    }

    #[test]
    fn a_status_press_without_a_percentage_plays_nothing() {
        assert_eq!(status(None), None);
        assert_eq!(status(Some(40)), Some(battery_quarters(40)));
        assert_eq!(status(Some(0)).map(|p| p.name), Some("battery-1"));
    }

    #[test]
    fn the_error_flicker_is_bounded_at_two_seconds() {
        let e = error();
        assert_eq!(e.name, "error");
        assert_eq!(e.steps.len(), 20);
        assert_eq!(ons(&e), [50; ERROR_FLASHES]);
        assert!(e.steps.iter().all(|s| s.off_ms == 50));
        assert_eq!(e.total_ms(), 20 * 50 + 19 * 50);
    }

    #[test]
    fn notify_is_one_short_and_release_now_one_blip() {
        let n = notify();
        assert_eq!(n.name, "notify");
        assert_eq!(n.steps, &[SHORT]);
        assert_eq!(n.total_ms(), 150);
        let r = release_now();
        assert_eq!(r.name, "release-now");
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.total_ms(), 60);
    }

    #[test]
    fn report_delivered_is_two_short() {
        let d = report_delivered();
        assert_eq!(d.name, "report-delivered");
        assert_eq!(d.steps, &[SHORT, SHORT]);
        assert_eq!(d.total_ms(), 650);
        assert_ne!(d.steps, notify().steps, "not the inbound message's one");
    }

    #[test]
    fn announce_sent_is_one_short_then_one_blip() {
        let a = announce_sent();
        assert_eq!(a.name, "announce-sent");
        assert_eq!(
            a.steps,
            &[
                Step {
                    on_ms: 150,
                    off_ms: 150
                },
                Step {
                    on_ms: 60,
                    off_ms: 0
                }
            ]
        );
        assert_eq!(a.total_ms(), 360);
        // Apart from every pattern it could be heard as.
        for other in [
            battery_quarters(25),
            notify(),
            release_now(),
            report_delivered(),
        ] {
            assert_ne!(a.steps, other.steps, "{}", other.name);
        }
        assert_ne!(a.total_ms(), report_delivered().total_ms());
    }

    fn sent() -> Option<&'static str> {
        Some("announce-sent")
    }

    fn name(p: Option<Pattern>) -> Option<&'static str> {
        p.map(|p| p.name)
    }

    #[test]
    fn a_press_plays_once_when_its_last_packet_is_keyed() {
        let mut w = AirWatch::new();
        w.open();
        // Two announces: packets 6 and 7.
        assert_eq!(w.close(5, 7, true, true), None);
        assert_eq!(w, AirWatch::Armed { packet: 7 });
        assert_eq!(name(w.settled(6, true)), None, "not per announce");
        assert_eq!(name(w.settled(7, true)), sent());
        assert_eq!(name(w.settled(8, true)), None, "once per press");
        assert_eq!(w, AirWatch::Idle);
    }

    #[test]
    fn a_press_whose_last_packet_was_not_keyed_plays_nothing() {
        let mut w = AirWatch::new();
        w.open();
        assert_eq!(w.close(0, 2, true, true), None);
        assert_eq!(name(w.settled(1, true)), None);
        assert_eq!(name(w.settled(2, false)), None);
        assert_eq!(w, AirWatch::Idle);
    }

    #[test]
    fn a_packet_settled_before_the_close_is_resolved_by_it() {
        let mut w = AirWatch::new();
        w.open();
        assert_eq!(name(w.settled(1, true)), None);
        assert_eq!(name(w.settled(2, true)), None);
        assert_eq!(name(w.close(0, 2, true, true)), sent());
        assert_eq!(w, AirWatch::Idle);
        assert_eq!(name(w.settled(3, true)), None);

        let mut w = AirWatch::new();
        w.open();
        assert_eq!(name(w.settled(2, false)), None);
        assert_eq!(name(w.close(0, 2, true, true)), None, "dropped, silent");
        assert_eq!(w, AirWatch::Idle);
    }

    #[test]
    fn an_earlier_packet_settled_before_the_close_still_arms() {
        let mut w = AirWatch::new();
        w.open();
        assert_eq!(name(w.settled(1, true)), None);
        assert_eq!(name(w.close(0, 2, true, true)), None);
        assert_eq!(w, AirWatch::Armed { packet: 2 });
        assert_eq!(name(w.settled(2, true)), sent());
    }

    #[test]
    fn no_lora_packet_plays_only_when_lora_is_off() {
        let mut w = AirWatch::new();
        w.open();
        assert_eq!(name(w.close(4, 4, true, false)), sent());
        w.open();
        assert_eq!(name(w.close(4, 4, true, true)), None, "queue refused");
        w.open();
        assert_eq!(name(w.close(4, 4, false, false)), None, "deferred");
        w.open();
        assert_eq!(name(w.close(4, 6, false, true)), None, "deferred");
        assert_eq!(w, AirWatch::Idle);
    }

    #[test]
    fn settles_without_a_press_play_nothing() {
        let mut w = AirWatch::new();
        for packet in 0..5 {
            assert_eq!(name(w.settled(packet, true)), None);
        }
        assert_eq!(w, AirWatch::Idle);
    }

    #[test]
    fn the_packet_numbers_wrap() {
        let mut w = AirWatch::new();
        w.open();
        assert_eq!(w.close(u32::MAX, 1, true, true), None);
        assert_eq!(name(w.settled(0, true)), None);
        assert_eq!(name(w.settled(1, true)), sent());
    }

    // The timings keep the signals apart. Checked at compile time, so a
    // constant moved past its reason does not build.
    const _: () = {
        // The gaps are what the eye counts.
        assert!(COUNT_GAP_MS >= 2 * SHORT_ON_MS);
        // A long cannot be read as a short, a blip not as a quarter.
        assert!(LONG_ON_MS >= 6 * SHORT_ON_MS);
        assert!(RELEASE_BLIP_MS < SHORT_ON_MS);
        // announce-sent's blip is under half a short and its gap under
        // half the counting one: a figure, not a count of two.
        assert!(2 * RELEASE_BLIP_MS < SHORT_ON_MS);
        assert!(2 * FIGURE_GAP_MS < COUNT_GAP_MS);
        // The flicker is far faster than the count and still visible.
        let count_period = SHORT_ON_MS + COUNT_GAP_MS;
        let flicker_period = 2 * FLICKER_MS;
        assert!(count_period >= 5 * flicker_period);
        assert!(1_000 / flicker_period < 50);
    };

    #[test]
    fn power_off_is_one_long_one_short_one_blip_each_shorter() {
        let p = power_off();
        assert_eq!(p.name, "power-off");
        let ons: [u16; 3] = [p.steps[0].on_ms, p.steps[1].on_ms, p.steps[2].on_ms];
        assert_eq!(ons, [LONG_ON_MS, SHORT_ON_MS, RELEASE_BLIP_MS]);
        assert!(ons[0] > ons[1] && ons[1] > ons[2]);
        // One figure: the gaps inside it are under half the counting gap.
        assert!(p.steps[..2].iter().all(|s| 2 * s.off_ms < COUNT_GAP_MS));
        assert_eq!(p.total_ms(), 1_000 + 150 + 150 + 150 + 60);
    }

    #[test]
    fn the_reset_warning_is_three_seconds_of_flicker() {
        let p = reset_warning();
        assert_eq!(p.steps.len(), RESET_WARNING_FLASHES);
        assert!(p.steps.iter().all(|s| *s == FLICKER));
        assert_eq!(p.total_ms(), 30 * 50 + 29 * 50);
        // Longer than the error, so the two are not the same signal.
        assert!(p.total_ms() > error().total_ms());
    }

    #[test]
    fn the_pattern_names_are_distinct_tokens() {
        let all = [
            mode(Mode::Tracker),
            mode(Mode::Mesh),
            battery_quarters(1),
            battery_quarters(30),
            battery_quarters(60),
            battery_quarters(90),
            error(),
            notify(),
            release_now(),
            report_delivered(),
            announce_sent(),
            power_off(),
            reset_warning(),
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(a
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
            for b in &all[i + 1..] {
                assert_ne!(a.name, b.name);
            }
        }
    }

    #[test]
    fn playback_walks_the_steps_and_skips_the_last_gap() {
        let p = Playback::new(battery_quarters(40), 1_000);
        assert_eq!(p.phase(1_000), Phase::On { until_ms: 1_150 });
        assert_eq!(p.phase(1_149), Phase::On { until_ms: 1_150 });
        assert_eq!(p.phase(1_150), Phase::Off { until_ms: 1_500 });
        assert_eq!(p.phase(1_499), Phase::Off { until_ms: 1_500 });
        assert_eq!(p.phase(1_500), Phase::On { until_ms: 1_650 });
        // The second blink is the last: no trailing gap.
        assert_eq!(p.phase(1_650), Phase::Done);
        assert_eq!(
            p.phase(1_000 + u64::from(p.pattern().total_ms())),
            Phase::Done
        );
    }

    #[test]
    fn a_late_reader_still_starts_lit() {
        let p = Playback::new(release_now(), 500);
        assert_eq!(p.phase(400), Phase::On { until_ms: 560 });
        assert_eq!(p.phase(560), Phase::Done);
    }

    #[test]
    fn every_pattern_ends_at_its_total_length() {
        for pattern in [
            mode(Mode::Tracker),
            mode(Mode::Mesh),
            battery_quarters(100),
            error(),
            notify(),
            release_now(),
            report_delivered(),
            announce_sent(),
            power_off(),
            reset_warning(),
        ] {
            let p = Playback::new(pattern, 0);
            let total = u64::from(pattern.total_ms());
            assert!(matches!(p.phase(total - 1), Phase::On { until_ms } if until_ms == total));
            assert_eq!(p.phase(total), Phase::Done, "{}", pattern.name);
        }
    }

    #[test]
    fn a_replacement_starts_from_its_own_first_step() {
        let first = Playback::new(battery_quarters(100), 0);
        assert!(matches!(first.phase(700), Phase::Off { .. }));
        let second = Playback::new(release_now(), 700);
        assert_eq!(second.phase(700), Phase::On { until_ms: 760 });
        assert_eq!(second.phase(760), Phase::Done);
    }
}
