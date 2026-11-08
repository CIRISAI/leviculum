//! The one-button feedback grammar's policy half (#459, order 892; moved
//! here in #469 973): which pattern a status press, a long press, a message
//! for the user, a delivered report and a double press's announce raise,
//! and on which sinks. The patterns, their timings and the air watch are
//! `leviculum-feedback`'s; the players that drive the pins with them stay
//! with the bind crate, behind [`Sinks`].
//!
//! What raises a pattern: [`Feedback::status`] (a short press, after the
//! display wake) and [`Feedback::release_now`] (a held button reaching
//! 2 s), both from the button's press handling ([`crate::button`]), which
//! holds the instance; [`notify`] (the
//! propagation role storing a message for this board's own `lxmf.delivery`
//! destination), [`report_delivered`] (the proof of the report a double
//! press sent, order 914) and the air watch of a double press
//! ([`announce_press_begin`], [`announce_press_end`], order 930), which
//! raises `announce-sent` when the LoRa task settles the press's last
//! announce keyed ([`lora_packet_settled`]). The mode blink
//! ([`leviculum_feedback::mode`]) has no caller until Tracker mode exists.
//!
//! # The sinks are registered once
//!
//! The policy is [`Feedback`], generic over its [`Sinks`], so a host test
//! holds one over `fakes::RecordingSinks` and its
//! own air watch. The board's one instance is [`Feedback`] over
//! [`Registered`], a zero-sized [`Sinks`] that forwards to the
//! implementation the bind crate registers with [`set_sinks`] at boot: the
//! callers that raise a pattern (the LoRa task, the propagation role, the
//! reporter) live in the bind crate today and in this crate after step 5,
//! and name only the free functions below, so neither they nor this crate
//! name a pin. The button's press handling ([`crate::button::run`]) is in
//! this crate since 976 and raises on the instance itself. The registration is the shape of
//! [`crate::entropy::set_source`]: two function pointers rather than a
//! `dyn`, which the seam rules exclude. A pattern raised before
//! [`set_sinks`] goes nowhere, as one raised on a sink the board does not
//! have always did.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use leviculum_feedback::{AirWatch, Pattern};

pub use crate::seams::Sinks;

/// Where a pattern plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// The board's LED.
    Led,
    /// The notification LED (Pocket V2, Solar Node).
    Notify,
    /// The buzzer (T1000-E).
    Buzzer,
}

impl Sink {
    /// The `sink=` token of the `FEEDBACK` line.
    pub fn as_str(self) -> &'static str {
        match self {
            Sink::Led => "led",
            Sink::Notify => "notify",
            Sink::Buzzer => "buzzer",
        }
    }
}

/// The feedback policy over one set of sinks, with the double press's
/// watch on the air.
pub struct Feedback<S> {
    sinks: S,
    // The node loop opens and closes the watch, the LoRa task, on its own
    // executor above the node loop, settles packets into it: a critical
    // section, not a signal, because both sides read and write the same
    // state.
    air_watch: Mutex<CriticalSectionRawMutex, Cell<AirWatch>>,
}

impl<S: Sinks> Feedback<S> {
    /// The policy over `sinks`, no press waiting.
    pub const fn new(sinks: S) -> Self {
        Self {
            sinks,
            air_watch: Mutex::new(Cell::new(AirWatch::new())),
        }
    }

    /// The sinks, for a test to read what they were given.
    #[cfg(test)]
    pub fn sinks(&self) -> &S {
        &self.sinks
    }

    fn raise(&self, sink: Sink, pattern: Pattern) {
        if self.sinks.present(sink) {
            self.sinks.play(sink, pattern);
        }
    }

    fn raise_led_and_buzzer(&self, pattern: Pattern) {
        self.raise(Sink::Led, pattern);
        self.raise(Sink::Buzzer, pattern);
    }

    /// A short press, `pct` the battery percentage if the board has one:
    /// the battery in quarters on the LED and the buzzer. Without one it
    /// plays nothing (a rig board on USB without a pack is not in error)
    /// and writes one `FEEDBACK pattern=none reason=no-percentage sink=led`
    /// line instead, so the rig tells "quiet by rule" from "the hook did
    /// not fire".
    pub fn status(&self, pct: Option<u8>) {
        match leviculum_feedback::status(pct) {
            Some(pattern) => self.raise_led_and_buzzer(pattern),
            None => crate::log::log_fmt(
                "",
                format_args!("FEEDBACK pattern=none reason=no-percentage sink=led"),
            ),
        }
    }

    /// A held button has reached the long press: one blip on the LED and
    /// the buzzer.
    pub fn release_now(&self) {
        self.raise_led_and_buzzer(leviculum_feedback::release_now());
    }

    /// A held button has reached the very long press (order 1511):
    /// `power-off` on the LED and the buzzer. The board switches off at the
    /// release.
    pub fn power_off(&self) {
        self.raise_led_and_buzzer(leviculum_feedback::power_off());
    }

    /// A message for the user arrived: one short on the notification LED
    /// where the board has one, else on the LED, and one short buzz where
    /// there is a buzzer.
    pub fn notify(&self) {
        let pattern = leviculum_feedback::notify();
        if self.sinks.present(Sink::Notify) {
            self.raise(Sink::Notify, pattern);
        } else {
            self.raise(Sink::Led, pattern);
        }
        self.raise(Sink::Buzzer, pattern);
    }

    /// The report a double press sent was proven delivered (order 914): two
    /// short on the LED and the buzzer, the sinks the press's own status
    /// answer uses. A report whose proof never comes plays nothing.
    pub fn report_delivered(&self) {
        self.raise_led_and_buzzer(leviculum_feedback::report_delivered());
    }

    fn with_air_watch(&self, f: impl FnOnce(&mut AirWatch) -> Option<Pattern>) {
        let pattern = self.air_watch.lock(|cell| {
            let mut watch = cell.get();
            let pattern = f(&mut watch);
            cell.set(watch);
            pattern
        });
        if let Some(pattern) = pattern {
            self.raise_led_and_buzzer(pattern);
        }
    }

    /// A double press is about to dispatch its announces (order 930). The
    /// caller reads the LoRa accept count after this, for
    /// [`announce_press_end`](Self::announce_press_end).
    pub fn announce_press_begin(&self) {
        self.with_air_watch(|watch| {
            watch.open();
            None
        });
    }

    /// The double press's dispatch is done. `lora_before` and `lora_after`
    /// are the LoRa accept count around it, `announced` is whether the
    /// press made any announce, `lora_active` whether the LoRa carrier is
    /// on. `announce-sent` plays on the LED and the buzzer when the last
    /// LoRa packet the dispatch queued has been keyed, at once if it
    /// already was, and at once when LoRa is off and the announces went to
    /// the other carriers only (`leviculum_feedback::AirWatch::close`).
    pub fn announce_press_end(
        &self,
        lora_before: u32,
        lora_after: u32,
        announced: bool,
        lora_active: bool,
    ) {
        self.with_air_watch(|watch| watch.close(lora_before, lora_after, announced, lora_active));
    }

    /// The LoRa task settled packet number `packet`, keyed or not.
    pub fn lora_packet_settled(&self, packet: u32, keyed: bool) {
        self.with_air_watch(|watch| watch.settled(packet, keyed));
    }
}

/// What a bind crate registers: its [`Sinks`] implementation's two methods,
/// as function pointers.
#[derive(Clone, Copy)]
struct Table {
    present: fn(Sink) -> bool,
    play: fn(Sink, Pattern),
}

static SINKS: Mutex<CriticalSectionRawMutex, Cell<Option<Table>>> = Mutex::new(Cell::new(None));

fn present_of<S: Sinks + Default>(sink: Sink) -> bool {
    S::default().present(sink)
}

fn play_of<S: Sinks + Default>(sink: Sink, pattern: Pattern) {
    S::default().play(sink, pattern)
}

/// Register the board's sinks, once, at boot. `S` is the bind crate's
/// zero-sized [`Sinks`] over its players. A second call is a programming
/// error, as for [`crate::entropy::set_source`]: debug builds assert it, a
/// release build keeps the later one.
pub fn set_sinks<S: Sinks + Default>() {
    let previous = SINKS.lock(|cell| {
        cell.replace(Some(Table {
            present: present_of::<S>,
            play: play_of::<S>,
        }))
    });
    debug_assert!(previous.is_none(), "feedback::set_sinks called twice");
}

/// The sinks [`set_sinks`] registered, as a zero-sized [`Sinks`]. Before
/// the registration no sink is present.
#[derive(Clone, Copy, Debug, Default)]
pub struct Registered;

impl Sinks for Registered {
    fn present(&self, sink: Sink) -> bool {
        SINKS
            .lock(|cell| cell.get())
            .is_some_and(|table| (table.present)(sink))
    }

    fn play(&self, sink: Sink, pattern: Pattern) {
        if let Some(table) = SINKS.lock(|cell| cell.get()) {
            (table.play)(sink, pattern);
        }
    }
}

/// The board's one instance, which the free functions below and the
/// button's press handling ([`crate::button::run`]) raise on.
pub(crate) static FEEDBACK: Feedback<Registered> = Feedback::new(Registered);

/// The battery percentage a short press reports, from
/// `board_state::BATTERY_STATE`. A board without the `battery`
/// capability, or a pack outside the band its cell count implies, has
/// none.
pub(crate) fn battery_percent() -> Option<u8> {
    #[cfg(feature = "battery")]
    let pct = crate::board_state::BATTERY_STATE
        .try_get()
        .and_then(|state| state.percent);
    #[cfg(not(feature = "battery"))]
    let pct: Option<u8> = None;
    pct
}

/// A message for the user arrived ([`Feedback::notify`]).
pub fn notify() {
    FEEDBACK.notify();
}

/// The report a double press sent was proven delivered
/// ([`Feedback::report_delivered`]).
pub fn report_delivered() {
    FEEDBACK.report_delivered();
}

/// A double press is about to dispatch its announces
/// ([`Feedback::announce_press_begin`]).
pub fn announce_press_begin() {
    FEEDBACK.announce_press_begin();
}

/// The double press's dispatch is done ([`Feedback::announce_press_end`]).
pub fn announce_press_end(lora_before: u32, lora_after: u32, announced: bool, lora_active: bool) {
    FEEDBACK.announce_press_end(lora_before, lora_after, announced, lora_active);
}

/// The LoRa task settled packet number `packet`, keyed or not
/// ([`Feedback::lora_packet_settled`]).
pub fn lora_packet_settled(packet: u32, keyed: bool) {
    FEEDBACK.lora_packet_settled(packet, keyed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::RecordingSinks;
    use alloc::vec;
    use alloc::vec::Vec;

    /// A board with these sinks.
    fn board(sinks: &[Sink]) -> Feedback<RecordingSinks> {
        Feedback::new(RecordingSinks::new(sinks))
    }

    /// What the sinks were given: `(t_ms, sink, pattern=)`.
    fn played(feedback: &Feedback<RecordingSinks>) -> Vec<(u64, Sink, &'static str)> {
        feedback.sinks().calls()
    }

    const LED_AND_BUZZER: &[Sink] = &[Sink::Led, Sink::Buzzer];

    // `status`: `Some(pattern) => { raise(Sink::Led, pattern);
    // raise(Sink::Buzzer, pattern); }`, `None =>` the log line and nothing
    // raised (the bind crate's `feedback.rs:124-134` before 973).
    #[test]
    fn status_plays_the_quarters_on_led_and_buzzer() {
        let feedback = board(LED_AND_BUZZER);
        feedback.sinks().advance(1_000);
        feedback.status(Some(60));
        let pattern = leviculum_feedback::battery_quarters(60).name;
        assert_eq!(
            played(&feedback),
            vec![(1_000, Sink::Led, pattern), (1_000, Sink::Buzzer, pattern)]
        );
    }

    #[test]
    fn status_without_a_percentage_plays_nothing() {
        let feedback = board(LED_AND_BUZZER);
        feedback.status(None);
        assert_eq!(played(&feedback), vec![]);
    }

    // `release_now`: `raise(Sink::Led, pattern); raise(Sink::Buzzer,
    // pattern);` (before 973 `:140-144`). A board without a buzzer is not
    // asked for one.
    #[test]
    fn release_now_blips_led_and_buzzer_and_skips_an_absent_sink() {
        let feedback = board(&[Sink::Led]);
        feedback.sinks().advance(2_000);
        feedback.release_now();
        assert_eq!(played(&feedback), vec![(2_000, Sink::Led, "release-now")]);
    }

    // `notify`: the notification LED where present, else the LED, then
    // the buzzer (before 973 `:149-157`).
    #[test]
    fn notify_prefers_the_notification_led() {
        let feedback = board(&[Sink::Led, Sink::Notify, Sink::Buzzer]);
        feedback.notify();
        assert_eq!(
            played(&feedback),
            vec![(0, Sink::Notify, "notify"), (0, Sink::Buzzer, "notify")]
        );
    }

    #[test]
    fn notify_falls_back_to_the_led() {
        let feedback = board(LED_AND_BUZZER);
        feedback.notify();
        assert_eq!(
            played(&feedback),
            vec![(0, Sink::Led, "notify"), (0, Sink::Buzzer, "notify")]
        );
    }

    // `report_delivered`: LED and buzzer (before 973 `:162-166`).
    #[test]
    fn report_delivered_plays_on_led_and_buzzer() {
        let feedback = board(LED_AND_BUZZER);
        feedback.sinks().advance(5);
        feedback.report_delivered();
        assert_eq!(
            played(&feedback),
            vec![
                (5, Sink::Led, "report-delivered"),
                (5, Sink::Buzzer, "report-delivered")
            ]
        );
    }

    // The air watch: `announce_press_begin` opens it (before 973
    // `:191-197`), `announce_press_end` closes it over the accept count
    // around the dispatch (`:204-208`), and `lora_packet_settled` (`:211`)
    // plays `announce-sent` on the LED and the buzzer when the press's
    // last packet settles keyed, at the settle and not at the press.
    #[test]
    fn announce_sent_plays_when_the_last_packet_is_keyed() {
        let feedback = board(LED_AND_BUZZER);
        feedback.announce_press_begin();
        feedback.announce_press_end(4, 6, true, true);
        feedback.lora_packet_settled(5, true);
        assert_eq!(played(&feedback), vec![], "not the press's last packet");
        feedback.sinks().advance(700);
        feedback.lora_packet_settled(6, true);
        assert_eq!(
            played(&feedback),
            vec![
                (700, Sink::Led, "announce-sent"),
                (700, Sink::Buzzer, "announce-sent")
            ]
        );
    }

    #[test]
    fn announce_sent_plays_at_the_end_when_the_packet_already_went() {
        let feedback = board(LED_AND_BUZZER);
        feedback.announce_press_begin();
        feedback.lora_packet_settled(9, true);
        feedback.sinks().advance(40);
        feedback.announce_press_end(8, 9, true, true);
        assert_eq!(
            played(&feedback),
            vec![
                (40, Sink::Led, "announce-sent"),
                (40, Sink::Buzzer, "announce-sent")
            ]
        );
    }

    #[test]
    fn an_unkeyed_last_packet_plays_nothing() {
        let feedback = board(LED_AND_BUZZER);
        feedback.announce_press_begin();
        feedback.announce_press_end(0, 1, true, true);
        feedback.lora_packet_settled(1, false);
        assert_eq!(played(&feedback), vec![]);
    }

    #[test]
    fn announces_off_lora_play_at_the_end() {
        let feedback = board(LED_AND_BUZZER);
        feedback.announce_press_begin();
        feedback.announce_press_end(3, 3, true, false);
        assert_eq!(
            played(&feedback),
            vec![
                (0, Sink::Led, "announce-sent"),
                (0, Sink::Buzzer, "announce-sent")
            ]
        );
    }

    #[test]
    fn a_settle_with_no_press_waiting_plays_nothing() {
        let feedback = board(LED_AND_BUZZER);
        feedback.lora_packet_settled(1, true);
        assert_eq!(played(&feedback), vec![]);
    }

    // The board's instance before any registration: nothing is present,
    // nothing plays. Only this test touches the registration static.
    #[test]
    fn an_unregistered_board_has_no_sink() {
        assert!(!Registered.present(Sink::Led));
        Registered.play(Sink::Led, leviculum_feedback::release_now());
    }
}
