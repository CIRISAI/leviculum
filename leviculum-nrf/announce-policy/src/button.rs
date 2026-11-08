//! What a press of a board's user button means (Codeberg #459, the
//! gestures of `docs/src/concepts/one-button.md`).
//!
//! A settled press becomes one of four gestures ([`Gesture`]): a short
//! press, a double press, a long press (held [`LONG_PRESS_MIN_MS`] or
//! more) and a very long one (held [`HOLD8_MIN_MS`] or more). The double
//! press sends the board's own announces, the delivery destination and the
//! propagation node destination, so somebody holding the board can make it
//! findable now instead of waiting out the cadence. The short press is
//! status; the long and the very long press are recognised and reported,
//! and get their meaning from the passes that build the mode switch and the
//! power off.
//!
//! Three gates stand between a contact and a gesture, and all three are
//! here because all three are timing that a thumb on a board cannot
//! reproduce:
//!
//! * [`Debouncer`]: a contact chatters for a few milliseconds on each edge,
//!   and every chatter is a GPIOTE edge. A level counts once it has held
//!   for [`BUTTON_DEBOUNCE_MS`] with no further edge.
//! * The double-press window: a short press is reported only once
//!   [`DOUBLE_PRESS_WINDOW_MS`] has passed after its release without a
//!   second press, so a double is never first reported as a short.
//! * [`PressLimiter`]: a double press announces at most once every
//!   [`BUTTON_ANNOUNCE_MIN_INTERVAL_MS`]; one inside the window is reported
//!   and dropped, never queued.
//!
//! [`ButtonGate`] is the three in the order the board runs them, and the
//! board does nothing with a press that this module did not decide.

/// How long a level must hold with no further edge before it counts.
///
/// Mechanical tact switches of the kind on these boards bounce for a few
/// milliseconds, rarely past 10 ms, and a press a person means lasts well
/// over 100 ms. Fifty sits between the two with margin on both sides: long
/// enough that the worst bounce is absorbed, short enough that no deliberate
/// press is mistaken for one. One constant for every board, because the
/// switches are the same kind and a per-board value would be a guess per
/// board.
pub const BUTTON_DEBOUNCE_MS: u64 = 50;

/// At most one press-triggered announce in this window (Lew, 2026-10-20).
///
/// The press is a person asking to be found, and the first announce answers
/// it; a second within half a minute reaches the same neighbours with the
/// same content. A thumb repeating the press, or a button pressed in a
/// pocket, would otherwise put one announce set on the air per press. The
/// window is fixed from the last announcing press, so a press inside it
/// does not extend it.
pub const BUTTON_ANNOUNCE_MIN_INTERVAL_MS: u64 = 30 * 1_000;

/// The longest gap between the release of a short press and the start of
/// the next press that still makes the two one double press.
///
/// Measured from the first edge of the release to the first edge of the
/// second press, so the debounce does not eat into it. A deliberate double
/// press on a tact switch leaves 100 to 250 ms between the two; desktop
/// systems default their double click to 500 ms for a mouse, whose button
/// travels less than a board's. The window is also what every short press
/// costs in latency, since a short is reported only once it has passed, so
/// it is kept below half a second: 400 ms takes the slow double with room
/// and keeps the status answer prompt.
pub const DOUBLE_PRESS_WINDOW_MS: u64 = 400;

/// The shortest press that is a long one (the mode switch of
/// `one-button.md`), and therefore neither a short nor a double. The
/// moment a held button reaches it is reported ([`ButtonEvent::Held`]) so
/// the rig can see it; the user's "release now" signal is the feedback
/// grammar's.
pub const LONG_PRESS_MIN_MS: u64 = 2_000;

/// The shortest press that is a very long one (the power off of
/// `one-button.md`). Reported when reached like [`LONG_PRESS_MIN_MS`].
pub const HOLD8_MIN_MS: u64 = 8_000;

/// The held thresholds, in the order a held button reaches them.
const HOLD_MARKS_MS: [u64; 2] = [LONG_PRESS_MIN_MS, HOLD8_MIN_MS];

/// A level the debouncer has accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// The button went down and stayed down.
    Down,
    /// The button came up after being down for `held_ms`, measured from the
    /// first edge of the press to the first edge of the release.
    Up {
        /// How long the button was held.
        held_ms: u64,
    },
    /// Edges arrived on a released button and it settled released again:
    /// a glitch, not a press.
    Bounce,
}

/// Accepts a level once it has held for [`BUTTON_DEBOUNCE_MS`] with no
/// further edge.
///
/// Fed by the GPIO task: [`edge`](Self::edge) on every edge it wakes for,
/// [`settle`](Self::settle) when [`deadline_ms`](Self::deadline_ms) has
/// passed. `pressed` is always the pin as read at that moment, so an edge
/// pair too fast for the task to see both halves of still restarts the
/// quiet period, and a level that changed while nothing woke is caught at
/// the settle.
#[derive(Debug, Clone, Copy, Default)]
pub struct Debouncer {
    /// The level as last read.
    raw: bool,
    /// The level last accepted.
    stable: bool,
    /// When the quiet period ends, `None` while nothing is pending.
    quiet_until_ms: Option<u64>,
    /// The first edge of the pending burst: the moment a press or a
    /// release actually began.
    burst_start_ms: u64,
    /// When the accepted press began.
    pressed_at_ms: u64,
}

impl Debouncer {
    /// A released button with nothing pending.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            raw: false,
            stable: false,
            quiet_until_ms: None,
            burst_start_ms: 0,
            pressed_at_ms: 0,
        }
    }

    /// An edge woke the task and the pin now reads `pressed`: restart the
    /// quiet period.
    pub fn edge(&mut self, now_ms: u64, pressed: bool) {
        if self.quiet_until_ms.is_none() {
            self.burst_start_ms = now_ms;
        }
        self.raw = pressed;
        self.quiet_until_ms = Some(now_ms.saturating_add(BUTTON_DEBOUNCE_MS));
    }

    /// When [`settle`](Self::settle) is next worth calling, `None` when
    /// nothing is pending and only an edge can change anything.
    #[must_use]
    pub const fn deadline_ms(&self) -> Option<u64> {
        self.quiet_until_ms
    }

    /// The first edge of the burst being debounced, `None` while nothing
    /// is pending.
    #[must_use]
    pub const fn pending_since_ms(&self) -> Option<u64> {
        match self.quiet_until_ms {
            Some(_) => Some(self.burst_start_ms),
            None => None,
        }
    }

    /// When the accepted press began: the first edge of its burst.
    #[must_use]
    pub const fn pressed_at_ms(&self) -> u64 {
        self.pressed_at_ms
    }

    /// The quiet period may have ended and the pin reads `pressed`.
    ///
    /// `None` while it has not ended, and when the burst left the accepted
    /// level where it was on a held button (contact chatter while held is
    /// not an event).
    pub fn settle(&mut self, now_ms: u64, pressed: bool) -> Option<Settled> {
        let until = self.quiet_until_ms?;
        if pressed != self.raw {
            // A change no edge woke us for: it is an edge, and it restarts
            // the quiet period like one.
            self.edge(now_ms, pressed);
            return None;
        }
        if now_ms < until {
            return None;
        }
        self.quiet_until_ms = None;
        if self.raw == self.stable {
            return (!self.stable).then_some(Settled::Bounce);
        }
        self.stable = self.raw;
        if self.stable {
            self.pressed_at_ms = self.burst_start_ms;
            Some(Settled::Down)
        } else {
            Some(Settled::Up {
                held_ms: self.burst_start_ms.saturating_sub(self.pressed_at_ms),
            })
        }
    }
}

/// One announcing press per [`BUTTON_ANNOUNCE_MIN_INTERVAL_MS`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PressLimiter {
    last_announce_ms: Option<u64>,
}

impl PressLimiter {
    /// No press yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_announce_ms: None,
        }
    }

    /// An announcing press at `now_ms`: `true` when it announces, which
    /// also starts the window; `false` inside the window, which leaves it
    /// alone.
    pub fn press(&mut self, now_ms: u64) -> bool {
        if let Some(last) = self.last_announce_ms {
            if now_ms < last.saturating_add(BUTTON_ANNOUNCE_MIN_INTERVAL_MS) {
                return false;
            }
        }
        self.last_announce_ms = Some(now_ms);
        true
    }
}

/// The four gestures of `one-button.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// One press under [`LONG_PRESS_MIN_MS`] with no second one inside
    /// [`DOUBLE_PRESS_WINDOW_MS`].
    Short,
    /// A short press and a second press that starts inside
    /// [`DOUBLE_PRESS_WINDOW_MS`] of its release.
    Double,
    /// One press held [`LONG_PRESS_MIN_MS`] or more, under
    /// [`HOLD8_MIN_MS`].
    Long,
    /// One press held [`HOLD8_MIN_MS`] or more.
    Hold8,
}

/// What a finished press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressAction {
    /// A short press: status. Sends nothing.
    Status,
    /// A double press outside the rate-limit window: send the announces.
    Announce,
    /// A double press inside the window: report it, send nothing.
    RateLimited,
    /// Edges that settled released: not a press.
    Debounced,
    /// A long press. No action yet.
    Long,
    /// A very long press. No action yet.
    Hold8,
}

impl PressAction {
    /// The `action=` token of the board's `BTN_PRESS` line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Announce => "announce",
            Self::RateLimited => "rate-limited",
            Self::Debounced => "debounced",
            Self::Long => "long",
            Self::Hold8 => "hold8",
        }
    }

    /// The gesture the action answers, `None` for a glitch.
    #[must_use]
    pub const fn gesture(self) -> Option<Gesture> {
        match self {
            Self::Status => Some(Gesture::Short),
            Self::Announce | Self::RateLimited => Some(Gesture::Double),
            Self::Long => Some(Gesture::Long),
            Self::Hold8 => Some(Gesture::Hold8),
            Self::Debounced => None,
        }
    }
}

/// A press the gate has classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// What the board does with it.
    pub action: PressAction,
    /// How long the button was held: the press itself for a short, a long
    /// and a very long press, the second press for a double, `0` for
    /// [`PressAction::Debounced`].
    pub held_ms: u64,
}

/// What the gate reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonEvent {
    /// A finished gesture, or a glitch.
    Press(Press),
    /// A press still held has just reached `ms`, one of
    /// [`LONG_PRESS_MIN_MS`] and [`HOLD8_MIN_MS`]. The gesture itself is
    /// reported at the release.
    Held {
        /// The threshold reached.
        ms: u64,
    },
}

/// Where the gate is between presses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Released, nothing owed.
    Idle,
    /// The first press is down; `marks` of [`HOLD_MARKS_MS`] reported.
    FirstDown { marks: usize },
    /// A short press was released at `released_at_ms` and is reported as
    /// a short unless a second press starts inside the window.
    Window { released_at_ms: u64, held_ms: u64 },
    /// The second press of a double is down.
    SecondDown,
}

/// The debouncer, the double-press window and the rate limit, in the
/// order the board runs them.
///
/// The state machine: `Idle` goes to `FirstDown` when a press settles.
/// From `FirstDown` a release of [`LONG_PRESS_MIN_MS`] or more is a long
/// or a very long press at once; a shorter one opens the `Window`. A press
/// that starts inside the window goes to `SecondDown`, and its release is
/// the double. A window that closes with no press started is the short.
#[derive(Debug, Clone, Copy)]
pub struct ButtonGate {
    debounce: Debouncer,
    limit: PressLimiter,
    phase: Phase,
}

impl Default for ButtonGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonGate {
    /// A released button, no press yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            debounce: Debouncer::new(),
            limit: PressLimiter::new(),
            phase: Phase::Idle,
        }
    }

    /// See [`Debouncer::edge`].
    pub fn edge(&mut self, now_ms: u64, pressed: bool) {
        self.debounce.edge(now_ms, pressed);
    }

    /// The next held threshold, while the first press is down with no
    /// burst pending. A burst decides first: if it is the release, the
    /// press is classified by its length; if it is chatter, the threshold
    /// is reported once it settles.
    fn hold_deadline_ms(&self) -> Option<u64> {
        match self.phase {
            Phase::FirstDown { marks } if self.debounce.deadline_ms().is_none() => HOLD_MARKS_MS
                .get(marks)
                .map(|mark| self.debounce.pressed_at_ms().saturating_add(*mark)),
            _ => None,
        }
    }

    /// When the window closes, unless a burst that started inside it is
    /// still being debounced: that one may be the second press.
    fn window_deadline_ms(&self) -> Option<u64> {
        let Phase::Window { released_at_ms, .. } = self.phase else {
            return None;
        };
        let end = released_at_ms.saturating_add(DOUBLE_PRESS_WINDOW_MS);
        match self.debounce.pending_since_ms() {
            Some(since) if since <= end => None,
            _ => Some(end),
        }
    }

    /// When [`settle`](Self::settle) is next worth calling, `None` when
    /// only an edge can change anything.
    #[must_use]
    pub fn deadline_ms(&self) -> Option<u64> {
        [
            self.debounce.deadline_ms(),
            self.hold_deadline_ms(),
            self.window_deadline_ms(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A deadline may have passed and the pin reads `pressed`. One event
    /// at most per call; when several are due,
    /// [`deadline_ms`](Self::deadline_ms) is still in the past and the
    /// next call takes the next one.
    pub fn settle(&mut self, now_ms: u64, pressed: bool) -> Option<ButtonEvent> {
        if let Some(at) = self.hold_deadline_ms() {
            if now_ms >= at {
                if !pressed {
                    // Released with no edge seen: debounce it as one.
                    self.debounce.edge(now_ms, pressed);
                    return None;
                }
                let Phase::FirstDown { marks } = self.phase else {
                    return None;
                };
                self.phase = Phase::FirstDown { marks: marks + 1 };
                return Some(ButtonEvent::Held {
                    ms: HOLD_MARKS_MS[marks],
                });
            }
        }
        if let Some(end) = self.window_deadline_ms() {
            if now_ms >= end {
                return Some(self.close_window());
            }
        }
        let event = match self.debounce.settle(now_ms, pressed)? {
            Settled::Bounce => Self::press(PressAction::Debounced, 0),
            Settled::Down => return self.down(),
            Settled::Up { held_ms } => self.up(now_ms, held_ms)?,
        };
        Some(event)
    }

    fn press(action: PressAction, held_ms: u64) -> ButtonEvent {
        ButtonEvent::Press(Press { action, held_ms })
    }

    /// The window closed with no second press: the short it was holding.
    fn close_window(&mut self) -> ButtonEvent {
        let held_ms = match self.phase {
            Phase::Window { held_ms, .. } => held_ms,
            _ => 0,
        };
        self.phase = Phase::Idle;
        Self::press(PressAction::Status, held_ms)
    }

    fn down(&mut self) -> Option<ButtonEvent> {
        match self.phase {
            Phase::Window { released_at_ms, .. } => {
                let end = released_at_ms.saturating_add(DOUBLE_PRESS_WINDOW_MS);
                if self.debounce.pressed_at_ms() <= end {
                    self.phase = Phase::SecondDown;
                    None
                } else {
                    // Started after the window: the short stands, and this
                    // is the first press of whatever comes next.
                    let short = self.close_window();
                    self.phase = Phase::FirstDown { marks: 0 };
                    Some(short)
                }
            }
            _ => {
                self.phase = Phase::FirstDown { marks: 0 };
                None
            }
        }
    }

    fn up(&mut self, now_ms: u64, held_ms: u64) -> Option<ButtonEvent> {
        match self.phase {
            Phase::FirstDown { .. } => {
                if held_ms >= HOLD8_MIN_MS {
                    self.phase = Phase::Idle;
                    Some(Self::press(PressAction::Hold8, held_ms))
                } else if held_ms >= LONG_PRESS_MIN_MS {
                    self.phase = Phase::Idle;
                    Some(Self::press(PressAction::Long, held_ms))
                } else {
                    self.phase = Phase::Window {
                        released_at_ms: self.debounce.pressed_at_ms().saturating_add(held_ms),
                        held_ms,
                    };
                    None
                }
            }
            Phase::SecondDown => {
                self.phase = Phase::Idle;
                let action = if self.limit.press(now_ms) {
                    PressAction::Announce
                } else {
                    PressAction::RateLimited
                };
                Some(Self::press(action, held_ms))
            }
            Phase::Idle | Phase::Window { .. } => None,
        }
    }
}

/// How long the button must be held through the boot before the factory
/// reset warns (`one-button.md`, "button held at boot, 10 s"; order 1511).
pub const FACTORY_RESET_HOLD_MS: u64 = 10_000;

/// What a button held at boot asks for next ([`BootHold::step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootHoldStep {
    /// Keep watching; nothing changes before `until_ms` unless the level
    /// does.
    Hold {
        /// The next moment the answer can change on its own.
        until_ms: u64,
    },
    /// The hold is complete: play the warning now. It ends at `until_ms`,
    /// and letting go before then keeps everything.
    Warn {
        /// When the warning ends and the wipe is due.
        until_ms: u64,
    },
    /// The warning has played with the button still held: wipe.
    Wipe,
    /// The button was let go before the wipe: boot as if nothing was held.
    Abort {
        /// How long it was held, from the first look to the release.
        held_ms: u64,
    },
}

/// The factory reset's hold at boot (order 1511): the button held for
/// [`FACTORY_RESET_HOLD_MS`] from the first look, then a warning of
/// `warning_ms` (the feedback grammar's `reset-warning`), then the wipe if
/// it is still held. A release that lasts [`BUTTON_DEBOUNCE_MS`] at any
/// moment before the wipe aborts, so a bounce does not, and a short press
/// that woke a switched-off board is over long before the hold is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootHold {
    start_ms: u64,
    warning_ms: u64,
    warned_at: Option<u64>,
    released_at: Option<u64>,
}

impl BootHold {
    /// A button found pressed at `start_ms`.
    pub const fn new(start_ms: u64, warning_ms: u64) -> Self {
        Self {
            start_ms,
            warning_ms,
            warned_at: None,
            released_at: None,
        }
    }

    /// The level `pressed` read at `now_ms`. After [`BootHoldStep::Wipe`]
    /// or [`BootHoldStep::Abort`] the caller stops asking.
    pub fn step(&mut self, now_ms: u64, pressed: bool) -> BootHoldStep {
        if pressed {
            self.released_at = None;
        } else {
            let since = *self.released_at.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= BUTTON_DEBOUNCE_MS {
                return BootHoldStep::Abort {
                    held_ms: since.saturating_sub(self.start_ms),
                };
            }
        }
        let bounce_end = self.released_at.map(|at| at + BUTTON_DEBOUNCE_MS);
        let hold_end = self.start_ms + FACTORY_RESET_HOLD_MS;
        match self.warned_at {
            None if now_ms >= hold_end && pressed => {
                self.warned_at = Some(now_ms);
                BootHoldStep::Warn {
                    until_ms: now_ms + self.warning_ms,
                }
            }
            None => BootHoldStep::Hold {
                until_ms: bounce_end.map_or(hold_end, |at| at.min(hold_end)),
            },
            Some(at) if now_ms >= at + self.warning_ms && pressed => BootHoldStep::Wipe,
            Some(at) => BootHoldStep::Hold {
                until_ms: bounce_end.unwrap_or(at + self.warning_ms),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the gate the way the GPIO task does: each `(t, level)` is an
    /// edge, and between edges the task settles at every deadline that
    /// falls before the next one. The clock never runs backwards: a
    /// deadline already behind it is settled at the clock, as the task's
    /// timer would. Returns every event, with the moment it was reported.
    fn run(gate: &mut ButtonGate, edges: &[(u64, bool)], until_ms: u64) -> Vec<(u64, ButtonEvent)> {
        fn settle_until(
            gate: &mut ButtonGate,
            level: bool,
            clock: &mut u64,
            before: u64,
            out: &mut Vec<(u64, ButtonEvent)>,
        ) {
            while let Some(d) = gate.deadline_ms() {
                if d >= before {
                    break;
                }
                *clock = d.max(*clock);
                if let Some(e) = gate.settle(*clock, level) {
                    out.push((*clock, e));
                }
            }
        }
        let mut out = Vec::new();
        let mut level = false;
        let mut clock = 0;
        for &(t, pressed) in edges {
            settle_until(gate, level, &mut clock, t, &mut out);
            level = pressed;
            clock = t;
            gate.edge(t, pressed);
        }
        settle_until(gate, level, &mut clock, until_ms, &mut out);
        out
    }

    /// The finished presses only, without the held marks.
    fn presses(events: &[(u64, ButtonEvent)]) -> Vec<(u64, Press)> {
        events
            .iter()
            .filter_map(|&(t, e)| match e {
                ButtonEvent::Press(p) => Some((t, p)),
                ButtonEvent::Held { .. } => None,
            })
            .collect()
    }

    fn actions(events: &[(u64, ButtonEvent)]) -> Vec<PressAction> {
        presses(events).into_iter().map(|(_, p)| p.action).collect()
    }

    /// A clean press of `held` ms starting at `t`, bouncing three times on
    /// each edge within 6 ms.
    fn bouncy_press(t: u64, held: u64) -> Vec<(u64, bool)> {
        let up = t + held;
        vec![
            (t, true),
            (t + 2, false),
            (t + 4, true),
            (t + 6, false),
            (t + 7, true),
            (up, false),
            (up + 1, true),
            (up + 3, false),
            (up + 5, true),
            (up + 6, false),
        ]
    }

    /// Two bouncy presses of `held` ms, the second starting `gap` ms after
    /// the first release.
    fn bouncy_pair(t: u64, held: u64, gap: u64) -> Vec<(u64, bool)> {
        let mut edges = bouncy_press(t, held);
        edges.extend(bouncy_press(t + held + gap, held));
        edges
    }

    #[test]
    fn a_short_press_is_reported_once_the_window_has_passed() {
        let mut gate = ButtonGate::new();
        let events = run(&mut gate, &bouncy_press(1_000, 200), 10_000);
        assert_eq!(
            events,
            vec![(
                1_200 + DOUBLE_PRESS_WINDOW_MS,
                ButtonEvent::Press(Press {
                    action: PressAction::Status,
                    held_ms: 200,
                })
            )],
            "ten edges are one press, a short reported when the window from \
             the first edge of the release has closed, timed from the first \
             edge of each burst"
        );
        assert_eq!(PressAction::Status.gesture(), Some(Gesture::Short));
    }

    #[test]
    fn a_double_inside_the_window_is_one_double_never_a_short_first() {
        let mut gate = ButtonGate::new();
        let edges = bouncy_pair(1_000, 150, DOUBLE_PRESS_WINDOW_MS);
        let events = run(&mut gate, &edges, 100_000);
        let second_up = 1_000 + 150 + DOUBLE_PRESS_WINDOW_MS + 150;
        assert_eq!(
            events,
            vec![(
                second_up + 6 + BUTTON_DEBOUNCE_MS,
                ButtonEvent::Press(Press {
                    action: PressAction::Announce,
                    held_ms: 150,
                })
            )],
            "a second press starting on the window's last millisecond makes \
             the pair a double, reported at the second release, with bounces \
             on all four edges"
        );
        assert_eq!(PressAction::Announce.gesture(), Some(Gesture::Double));
    }

    #[test]
    fn two_shorts_outside_the_window_are_two_shorts() {
        let mut gate = ButtonGate::new();
        let edges = bouncy_pair(1_000, 150, DOUBLE_PRESS_WINDOW_MS + 1);
        let events = run(&mut gate, &edges, 100_000);
        assert_eq!(
            presses(&events),
            vec![
                (
                    1_150 + DOUBLE_PRESS_WINDOW_MS,
                    Press {
                        action: PressAction::Status,
                        held_ms: 150,
                    }
                ),
                (
                    1_150 + DOUBLE_PRESS_WINDOW_MS + 1 + 150 + DOUBLE_PRESS_WINDOW_MS,
                    Press {
                        action: PressAction::Status,
                        held_ms: 150,
                    }
                ),
            ]
        );
    }

    #[test]
    fn a_long_press_fires_on_release_after_its_mark() {
        let mut gate = ButtonGate::new();
        let held = LONG_PRESS_MIN_MS + 100;
        let mut edges = bouncy_press(1_000, held);
        edges.extend(bouncy_press(10_000, 100));
        let events = run(&mut gate, &edges, 100_000);
        assert_eq!(
            events,
            vec![
                (
                    1_000 + LONG_PRESS_MIN_MS,
                    ButtonEvent::Held {
                        ms: LONG_PRESS_MIN_MS
                    }
                ),
                (
                    1_000 + held + 6 + BUTTON_DEBOUNCE_MS,
                    ButtonEvent::Press(Press {
                        action: PressAction::Long,
                        held_ms: held,
                    })
                ),
                (
                    10_100 + DOUBLE_PRESS_WINDOW_MS,
                    ButtonEvent::Press(Press {
                        action: PressAction::Status,
                        held_ms: 100,
                    })
                ),
            ],
            "the mark at 2 s while held, the long at the release with no \
             window to wait out, and the next press is a short again"
        );
        let mut gate = ButtonGate::new();
        let events = run(
            &mut gate,
            &bouncy_press(1_000, LONG_PRESS_MIN_MS - 1),
            100_000,
        );
        assert_eq!(actions(&events), vec![PressAction::Status]);
        assert_eq!(events.len(), 1, "no mark below 2 s");
    }

    #[test]
    fn a_hold_reports_both_marks_and_fires_on_release() {
        let mut gate = ButtonGate::new();
        let events = run(&mut gate, &bouncy_press(1_000, 9_000), 100_000);
        assert_eq!(
            events,
            vec![
                (
                    1_000 + LONG_PRESS_MIN_MS,
                    ButtonEvent::Held {
                        ms: LONG_PRESS_MIN_MS
                    }
                ),
                (1_000 + HOLD8_MIN_MS, ButtonEvent::Held { ms: HOLD8_MIN_MS }),
                (
                    10_000 + 6 + BUTTON_DEBOUNCE_MS,
                    ButtonEvent::Press(Press {
                        action: PressAction::Hold8,
                        held_ms: 9_000,
                    })
                ),
            ]
        );
        let mut gate = ButtonGate::new();
        let events = run(&mut gate, &bouncy_press(0, HOLD8_MIN_MS - 1), 100_000);
        assert_eq!(actions(&events), vec![PressAction::Long]);
        assert_eq!(PressAction::Hold8.gesture(), Some(Gesture::Hold8));
    }

    #[test]
    fn a_release_bursting_before_the_mark_is_not_held() {
        // The release's first edge lands 1 ms before the mark and its
        // quiet period runs past it: no mark, a short.
        let mut gate = ButtonGate::new();
        let events = run(
            &mut gate,
            &[(0, true), (LONG_PRESS_MIN_MS - 1, false)],
            100_000,
        );
        assert_eq!(actions(&events), vec![PressAction::Status]);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn a_bounce_inside_a_double_keeps_it_a_double() {
        // A glitch on the released button between the two presses, after
        // the first release has settled: it is reported as debounced, and the second press inside the window
        // still makes the pair one double.
        let mut gate = ButtonGate::new();
        let mut edges = bouncy_press(1_000, 150);
        edges.extend([(1_260, true), (1_270, false)]);
        edges.extend(bouncy_press(1_400, 150));
        let events = run(&mut gate, &edges, 100_000);
        assert_eq!(
            actions(&events),
            vec![PressAction::Debounced, PressAction::Announce]
        );
    }

    #[test]
    fn the_rate_limit_applies_to_the_double() {
        let mut gate = ButtonGate::new();
        let mut edges = bouncy_pair(1_000, 150, 200);
        edges.extend(bouncy_pair(11_000, 150, 200));
        edges.extend(bouncy_pair(
            1_000 + BUTTON_ANNOUNCE_MIN_INTERVAL_MS + 1_000,
            150,
            200,
        ));
        let events = run(&mut gate, &edges, 100_000);
        assert_eq!(
            actions(&events),
            vec![
                PressAction::Announce,
                PressAction::RateLimited,
                PressAction::Announce,
            ]
        );
        assert_eq!(PressAction::RateLimited.gesture(), Some(Gesture::Double));
    }

    #[test]
    fn shorts_and_longs_leave_the_rate_limit_window_free() {
        let mut gate = ButtonGate::new();
        let mut edges = bouncy_press(1_000, 150);
        edges.extend(bouncy_press(3_000, LONG_PRESS_MIN_MS));
        edges.extend(bouncy_pair(8_000, 150, 200));
        let events = run(&mut gate, &edges, 100_000);
        assert_eq!(
            actions(&events),
            vec![
                PressAction::Status,
                PressAction::Long,
                PressAction::Announce
            ]
        );
    }

    #[test]
    fn a_glitch_shorter_than_the_window_is_debounced_not_a_press() {
        let mut gate = ButtonGate::new();
        let events = run(&mut gate, &[(500, true), (520, false)], 10_000);
        assert_eq!(
            events,
            vec![(
                520 + BUTTON_DEBOUNCE_MS,
                ButtonEvent::Press(Press {
                    action: PressAction::Debounced,
                    held_ms: 0,
                })
            )]
        );
        assert_eq!(PressAction::Debounced.gesture(), None);
        // And it did not open a window or spend the rate limit.
        let events = run(&mut gate, &bouncy_pair(1_000, 150, 200), 10_000);
        assert_eq!(actions(&events), vec![PressAction::Announce]);
    }

    #[test]
    fn a_dropped_press_does_not_extend_the_window() {
        let mut limit = PressLimiter::new();
        assert!(limit.press(0));
        assert!(!limit.press(BUTTON_ANNOUNCE_MIN_INTERVAL_MS - 1));
        assert!(limit.press(BUTTON_ANNOUNCE_MIN_INTERVAL_MS));
    }

    #[test]
    fn chatter_while_held_is_not_an_event() {
        let mut gate = ButtonGate::new();
        let events = run(
            &mut gate,
            &[(0, true), (300, false), (310, true), (900, false)],
            10_000,
        );
        assert_eq!(
            events,
            vec![(
                900 + DOUBLE_PRESS_WINDOW_MS,
                ButtonEvent::Press(Press {
                    action: PressAction::Status,
                    held_ms: 900,
                })
            )]
        );
    }

    #[test]
    fn chatter_across_the_mark_reports_it_once_it_settles() {
        let mut gate = ButtonGate::new();
        let events = run(
            &mut gate,
            &[(0, true), (1_990, false), (1_995, true), (3_000, false)],
            10_000,
        );
        assert_eq!(
            events,
            vec![
                (
                    1_995 + BUTTON_DEBOUNCE_MS,
                    ButtonEvent::Held {
                        ms: LONG_PRESS_MIN_MS
                    }
                ),
                (
                    3_000 + BUTTON_DEBOUNCE_MS,
                    ButtonEvent::Press(Press {
                        action: PressAction::Long,
                        held_ms: 3_000,
                    })
                ),
            ]
        );
    }

    #[test]
    fn a_change_no_edge_reported_restarts_the_quiet_period() {
        let mut d = Debouncer::new();
        d.edge(0, true);
        // At the deadline the pin reads released: the press half was never
        // quiet, so nothing is accepted and the wait starts again.
        assert_eq!(d.settle(BUTTON_DEBOUNCE_MS, false), None);
        assert_eq!(d.deadline_ms(), Some(2 * BUTTON_DEBOUNCE_MS));
        assert_eq!(
            d.settle(2 * BUTTON_DEBOUNCE_MS, false),
            Some(Settled::Bounce)
        );
        assert_eq!(d.deadline_ms(), None);
    }

    #[test]
    fn a_release_no_edge_reported_at_the_mark_is_debounced_as_one() {
        let mut gate = ButtonGate::new();
        gate.edge(0, true);
        assert_eq!(gate.settle(BUTTON_DEBOUNCE_MS, true), None);
        assert_eq!(gate.deadline_ms(), Some(LONG_PRESS_MIN_MS));
        // The pin reads released at the mark, and no edge woke the task.
        assert_eq!(gate.settle(LONG_PRESS_MIN_MS, false), None);
        assert_eq!(
            gate.deadline_ms(),
            Some(LONG_PRESS_MIN_MS + BUTTON_DEBOUNCE_MS)
        );
        assert_eq!(
            gate.settle(LONG_PRESS_MIN_MS + BUTTON_DEBOUNCE_MS, false),
            Some(ButtonEvent::Press(Press {
                action: PressAction::Long,
                held_ms: LONG_PRESS_MIN_MS,
            }))
        );
    }

    #[test]
    fn nothing_settles_before_the_quiet_period_ends() {
        let mut d = Debouncer::new();
        d.edge(100, true);
        assert_eq!(d.settle(100 + BUTTON_DEBOUNCE_MS - 1, true), None);
        assert_eq!(
            d.settle(100 + BUTTON_DEBOUNCE_MS, true),
            Some(Settled::Down)
        );
    }

    #[test]
    fn the_action_tokens_are_stable() {
        assert_eq!(PressAction::Status.as_str(), "status");
        assert_eq!(PressAction::Announce.as_str(), "announce");
        assert_eq!(PressAction::RateLimited.as_str(), "rate-limited");
        assert_eq!(PressAction::Debounced.as_str(), "debounced");
        assert_eq!(PressAction::Long.as_str(), "long");
        assert_eq!(PressAction::Hold8.as_str(), "hold8");
    }

    /// Walk a boot hold over `levels` (`(t_ms, pressed)`, each held until
    /// the next) at 10 ms ticks, the firmware's poll, and return what it
    /// asked for, warning start included.
    fn boot_hold(levels: &[(u64, bool)], until_ms: u64) -> (Option<u64>, BootHoldStep) {
        let mut hold = BootHold::new(0, 2_950);
        let mut warned = None;
        let mut t = 0;
        while t <= until_ms {
            let pressed = levels
                .iter()
                .rev()
                .find(|(at, _)| *at <= t)
                .is_some_and(|(_, p)| *p);
            match hold.step(t, pressed) {
                BootHoldStep::Warn { .. } => warned = Some(t),
                BootHoldStep::Hold { .. } => {}
                end => return (warned, end),
            }
            t += 10;
        }
        (warned, BootHoldStep::Hold { until_ms: t })
    }

    #[test]
    fn a_boot_hold_through_ten_seconds_and_the_warning_wipes() {
        let (warned, end) = boot_hold(&[(0, true)], 20_000);
        assert_eq!(warned, Some(FACTORY_RESET_HOLD_MS));
        assert_eq!(end, BootHoldStep::Wipe);
    }

    #[test]
    fn a_release_before_the_warning_aborts_and_says_how_long() {
        let (warned, end) = boot_hold(&[(0, true), (4_000, false)], 20_000);
        assert_eq!(warned, None);
        assert_eq!(end, BootHoldStep::Abort { held_ms: 4_000 });
    }

    #[test]
    fn a_release_during_the_warning_still_aborts() {
        let (warned, end) = boot_hold(&[(0, true), (11_000, false)], 20_000);
        assert_eq!(warned, Some(FACTORY_RESET_HOLD_MS));
        assert_eq!(end, BootHoldStep::Abort { held_ms: 11_000 });
    }

    #[test]
    fn a_bounce_shorter_than_the_debounce_does_not_abort() {
        let (warned, end) = boot_hold(&[(0, true), (5_000, false), (5_030, true)], 20_000);
        assert_eq!(warned, Some(FACTORY_RESET_HOLD_MS));
        assert_eq!(end, BootHoldStep::Wipe);
    }

    #[test]
    fn a_wake_press_is_over_long_before_the_hold() {
        // The press that wakes a switched-off board is still down at the
        // first look; it ends as an abort, and nothing is wiped.
        let (warned, end) = boot_hold(&[(0, true), (300, false)], 20_000);
        assert_eq!(warned, None);
        assert_eq!(end, BootHoldStep::Abort { held_ms: 300 });
    }
}
