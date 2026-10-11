//! The board reporter's own decisions, one tick at a time (#413).
//!
//! [`SendPolicy`] answers "report now, and why". Around that answer the
//! board's reporter (`leviculum-nrf/src/telemetry.rs`) takes a ring of
//! smaller decisions that used to be written inline: whether a target with
//! no position source may chase its key, how often the key is asked for,
//! when the flash record gains the resolved key, what an owed
//! retransmission does when its bytes are gone, which position a report
//! carries and from where, which failures after the pre-report announce
//! book its airtime, and how often a flash write is tried. Each of them
//! needed the three-radio delivery-promise topology plus an induced fault
//! to be seen on the rig (#344 spent three weeks there); here each is a
//! function of the values the board reads, and the reporter calls it.
//!
//! Nothing here knows a radio, a node or a flash part. The inputs are
//! what the board reads: presence state, fix validity, clock state, path
//! state, key state, flash results.

use crate::{choose_position, retry_step, Fix, PositionSource, RetryStep, SendPolicy, TargetState};

// ---------------------------------------------------------------------------
// The key chase
// ---------------------------------------------------------------------------

/// How often the node re-asks for a path while it is waiting for a
/// target's key. A path request is one small packet and the answer
/// carries the identity, so this is the key-resolution loop; 60 s keeps
/// it well under any announce cadence it might be racing.
pub const KEY_REQUEST_INTERVAL_MS: u64 = 60_000;

/// What the reporter does about its target before any report is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStep {
    /// A target and no position source: send nothing, not even the path
    /// request a key chase would cost.
    Silent,
    /// The target is usable; go on to the report.
    Proceed,
    /// The key the target was waiting for is now held: tell the policy,
    /// then go on.
    KeyArrived,
    /// No key, and the last request is at least
    /// [`KEY_REQUEST_INTERVAL_MS`] old (or there was none): ask for a path
    /// now and stop this tick.
    AskForKey,
    /// No key, asked recently: stop this tick.
    WaitForKey,
}

/// The key chase, from the target state, whether the node holds the
/// target's identity, and when it last asked for one.
///
/// [`TargetState::NoPositionSource`] comes first on purpose: a path
/// request is airtime spent chasing a key for reports that will never be
/// built.
pub const fn key_step(
    state: TargetState,
    key_known: bool,
    last_request_ms: Option<u64>,
    now_ms: u64,
) -> KeyStep {
    match state {
        TargetState::NoPositionSource => KeyStep::Silent,
        TargetState::AwaitingKey => {
            if key_known {
                return KeyStep::KeyArrived;
            }
            let due = match last_request_ms {
                None => true,
                Some(last) => now_ms.saturating_sub(last) >= KEY_REQUEST_INTERVAL_MS,
            };
            if due {
                KeyStep::AskForKey
            } else {
                KeyStep::WaitForKey
            }
        }
        _ => KeyStep::Proceed,
    }
}

/// Whether the target's flash record still owes the resolved key
/// (Codeberg #370).
///
/// The host sends hash-only targets and the identity store is RAM, so a
/// power cycle used to forget a key the node had already resolved. A
/// hash-only `Set` arms the debt; the first tick that holds the key pays
/// it, once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyRecordDebt {
    owed: bool,
}

impl KeyRecordDebt {
    /// No debt.
    pub const fn new() -> Self {
        Self { owed: false }
    }

    /// The debt a target frame leaves: owed iff it set a target without
    /// carrying its key. A frame that carried its key is persisted as
    /// sent, and a clear has no record to enrich.
    pub const fn after_target(sets_target: bool, carried_key: bool) -> Self {
        Self {
            owed: sets_target && !carried_key,
        }
    }

    /// Whether the debt is still open.
    pub const fn is_owed(&self) -> bool {
        self.owed
    }

    /// `true` exactly once: on the first call with the key held while the
    /// debt is open. That is the moment the record is rewritten.
    pub fn settle(&mut self, key_known: bool) -> bool {
        if self.owed && key_known {
            self.owed = false;
            return true;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// The owed retransmission
// ---------------------------------------------------------------------------

/// The owed retransmission's step, or `None` when the debt has to be
/// forgotten because its bytes are gone.
///
/// The payload is kept as long as the debt is, so `payload_kept == false`
/// is unreachable by construction; but a debt without bytes can only be
/// forgotten, not paid. With bytes it is [`retry_step`].
pub const fn owed_retry(payload_kept: bool, has_path: bool, path_asked: bool) -> Option<RetryStep> {
    if !payload_kept {
        return None;
    }
    Some(retry_step(has_path, path_asked))
}

// ---------------------------------------------------------------------------
// The position a report carries
// ---------------------------------------------------------------------------

/// Horizontal position error in metres per unit of HDOP.
///
/// The wire's accuracy slot is metres (`Location.pack`, Sideband
/// `2000d81`; Columba fills it from Android's `Location.accuracy`), and
/// standard NMEA carries no accuracy field at all — only HDOP, which is a
/// geometry factor and not a distance. Turning one into the other needs a
/// user-equivalent range error, and 5 m is the conventional open-sky
/// figure for a single-frequency consumer receiver.
///
/// This is a **model, stated as one**, in the sense
/// `docs/src/concepts/regulatory-airtime.md` fixes for any such figure:
/// it is not a measurement of this antenna in this housing, and a
/// deployment that has measured its own may say so. What it is not is a
/// placeholder — it is derived from a number the receiver actually
/// reported, and when the receiver reports no HDOP the position is not
/// sent at all ([`accuracy_m`]).
pub const HORIZONTAL_UERE_M: f32 = 5.0;

impl Fix {
    /// A fix from the receiver's decimal degrees and HDOP, or `None` when
    /// either coordinate is missing. The HDOP is optional here: a fix
    /// without one is still a position, which the accuracy gate then
    /// refuses ([`SendPolicy::position_is_reportable`]).
    pub fn from_degrees(
        latitude: Option<f64>,
        longitude: Option<f64>,
        hdop: Option<f32>,
    ) -> Option<Self> {
        let latitude = latitude?;
        let longitude = longitude?;
        Some(Self {
            latitude_e6: (latitude * 1e6) as i32,
            longitude_e6: (longitude * 1e6) as i32,
            hdop_e2: hdop.map(|h| (h * 100.0).clamp(0.0, 65535.0) as u16),
        })
    }
}

/// The accuracy a sensor location carries, in metres: HDOP times
/// [`HORIZONTAL_UERE_M`]. `None` without an HDOP, and then the sensor
/// location is not built at all: "we do not know how good this is" is
/// not a distance.
pub fn accuracy_m(hdop: Option<f32>) -> Option<f32> {
    Some(hdop? * HORIZONTAL_UERE_M)
}

/// The position a report is asked about, and its source.
///
/// Only a receiver in presence state `Fix` may contribute a sensor
/// position; a set fixed position replaces the sensor entirely, whatever
/// the receiver is doing ([`choose_position`]).
pub const fn report_position(
    presence_has_fix: bool,
    fixed: Option<Fix>,
    sensor: Option<Fix>,
) -> (Option<Fix>, PositionSource) {
    let sensor = if presence_has_fix { sensor } else { None };
    choose_position(fixed, sensor)
}

/// Where the location sensor of one report comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationSource {
    /// The report carries no position.
    None,
    /// The user's fixed position.
    Fixed,
    /// The GNSS readings.
    Sensor,
}

/// Which location a report carries, from whether the policy found the
/// position reportable, which source it came from, and whether a fixed
/// position is set. The fixed position is used only when it is both the
/// chosen source and set; every other reportable position is the
/// sensor's.
pub const fn location_source(
    include_position: bool,
    source: PositionSource,
    fixed_set: bool,
) -> LocationSource {
    match (include_position, source, fixed_set) {
        (false, ..) => LocationSource::None,
        (true, PositionSource::Fixed, true) => LocationSource::Fixed,
        (true, ..) => LocationSource::Sensor,
    }
}

impl SendPolicy {
    /// The position a report built now would carry: `fix` if it passes
    /// the accuracy gate, else none. The same value is what the cadence
    /// records as reported ([`note_emitted`](Self::note_emitted)), so an
    /// unreportable fix neither goes on the air nor moves the movement
    /// reference.
    pub const fn reportable_position(&self, fix: Option<Fix>) -> Option<Fix> {
        match fix {
            Some(f) if self.position_is_reportable(f) => Some(f),
            _ => None,
        }
    }

    /// Book what a withheld attempt cost: the pre-report announce, for
    /// every reason that comes after it ([`Withhold::books_airtime`]).
    pub const fn note_withheld(&mut self, now_ms: u64, reason: Withhold) {
        if reason.books_airtime() {
            self.note_airtime_spent(now_ms);
        }
    }
}

// ---------------------------------------------------------------------------
// Withheld attempts
// ---------------------------------------------------------------------------

/// Why a due report did not go out. The `reason=` slot of the
/// `[TELEMETRY] report withheld` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withhold {
    /// No calendar time yet: the timebase is uptime seconds, which lands
    /// the row in 1970 and repeats across reboots.
    NoClock,
    /// The report would not build.
    NoReadings,
    /// The message has no opportunistic on-air form.
    NoOnAir,
    /// No path entry for the target.
    NoPath,
    /// A path entry over an offline interface (#365).
    IfaceOffline,
    /// The core could not build or route the packet.
    SendFailed,
}

impl Withhold {
    /// The word on the log line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoClock => "no-clock",
            Self::NoReadings => "no-readings",
            Self::NoOnAir => "no-on-air",
            Self::NoPath => "no-path",
            Self::IfaceOffline => "iface-offline",
            Self::SendFailed => "send-failed",
        }
    }

    /// Whether the attempt had already spent the pre-report announce.
    /// Every reason but the clock comes after it: book it, or the next
    /// tick finds the floor clear and spends another (#236's flood).
    pub const fn books_airtime(self) -> bool {
        !matches!(self, Self::NoClock)
    }
}

/// The calendar second a report is stamped with, or the withhold when
/// the node has none.
pub const fn report_time(unix_secs: Option<u64>) -> Result<u64, Withhold> {
    match unix_secs {
        Some(secs) => Ok(secs),
        None => Err(Withhold::NoClock),
    }
}

/// Whether the report may be handed to the node: `None` with a usable
/// path, else the withhold that names why not. "No entry at all" and
/// "an entry over an offline interface" are told apart (#365); both ask
/// for a path.
pub const fn path_withhold(has_path: bool, route_held: bool) -> Option<Withhold> {
    match (has_path, route_held) {
        (true, _) => None,
        (false, true) => Some(Withhold::IfaceOffline),
        (false, false) => Some(Withhold::NoPath),
    }
}

// ---------------------------------------------------------------------------
// The flash write
// ---------------------------------------------------------------------------

/// How often a failed flash write is tried; the SoftDevice refuses flash
/// access while the radio is busy.
pub const SAVE_RETRIES: u8 = 3;
/// The pause after each failed flash write.
pub const SAVE_RETRY_MS: u64 = 250;

/// What follows one flash write attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStep {
    /// The page is written.
    Written,
    /// Attempt `attempt` of [`SAVE_RETRIES`] failed: pause `wait_ms`, then
    /// try again, or answer `Lost` when `gave_up`.
    Failed {
        attempt: u8,
        wait_ms: u64,
        gave_up: bool,
    },
}

/// The store task's retry loop over one page write: up to
/// [`SAVE_RETRIES`] attempts, [`SAVE_RETRY_MS`] after each failure, the
/// last one included.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlashSave {
    attempts: u8,
}

impl FlashSave {
    /// No attempt made yet.
    pub const fn new() -> Self {
        Self { attempts: 0 }
    }

    /// Book the outcome of the attempt just made.
    pub fn after_attempt(&mut self, written: bool) -> SaveStep {
        self.attempts = self.attempts.saturating_add(1);
        if written {
            return SaveStep::Written;
        }
        SaveStep::Failed {
            attempt: self.attempts,
            wait_ms: SAVE_RETRY_MS,
            gave_up: self.attempts >= SAVE_RETRIES,
        }
    }
}

// ---------------------------------------------------------------------------
// Inbound hand-over
// ---------------------------------------------------------------------------

/// Hand `item` to a depth-1 queue whose newest value is the only one
/// worth having: if the queue is full, the unread value is discarded and
/// `item` takes its place. Returns whether `item` was taken; `false` is a
/// queue that refused even after the discard, which a host's retry
/// covers.
///
/// The serial task hands targets and fixed positions to the main loop
/// this way, so a superseded frame never takes effect after the frame
/// that replaced it.
pub fn deliver_newest<T: Copy, E>(
    item: T,
    mut try_send: impl FnMut(T) -> Result<(), E>,
    mut discard_unread: impl FnMut(),
) -> bool {
    if try_send(item).is_ok() {
        return true;
    }
    discard_unread();
    try_send(item).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyParams, Profile, TargetCommand};

    // -- The key chase ----------------------------------------------------

    #[test]
    fn a_target_without_a_position_source_sends_nothing_not_even_a_key_request() {
        for key_known in [false, true] {
            assert_eq!(
                key_step(TargetState::NoPositionSource, key_known, None, 0),
                KeyStep::Silent
            );
        }
        // The state a fresh policy reaches: a hash-only target on a board
        // with neither a receiver nor a pin.
        let mut policy = SendPolicy::new();
        policy.apply(TargetCommand::Set(Profile::Tracker), false);
        assert_eq!(policy.state(), TargetState::NoPositionSource);
        assert_eq!(key_step(policy.state(), false, None, 0), KeyStep::Silent);
    }

    #[test]
    fn awaiting_key_asks_for_a_path_once_a_minute_until_the_key_is_known() {
        let t = 1_000_000;
        let state = TargetState::AwaitingKey;
        assert_eq!(key_step(state, false, None, t), KeyStep::AskForKey);
        assert_eq!(key_step(state, false, Some(t), t), KeyStep::WaitForKey);
        assert_eq!(
            key_step(state, false, Some(t), t + KEY_REQUEST_INTERVAL_MS - 1),
            KeyStep::WaitForKey
        );
        assert_eq!(
            key_step(state, false, Some(t), t + KEY_REQUEST_INTERVAL_MS),
            KeyStep::AskForKey
        );
        assert_eq!(key_step(state, true, Some(t), t), KeyStep::KeyArrived);
    }

    #[test]
    fn a_ready_target_goes_on_to_the_report() {
        for state in [TargetState::Ready, TargetState::Off] {
            assert_eq!(key_step(state, false, None, 0), KeyStep::Proceed);
        }
    }

    #[test]
    fn a_hash_only_target_rewrites_its_record_once_when_the_key_resolves() {
        let mut debt = KeyRecordDebt::after_target(true, false);
        assert!(debt.is_owed());
        assert!(!debt.settle(false), "no key yet, nothing to write");
        assert!(debt.settle(true), "the key is held: rewrite now");
        assert!(!debt.settle(true), "and only once");
        assert!(!debt.is_owed());
    }

    #[test]
    fn a_target_that_carried_its_key_or_a_clear_owes_no_rewrite() {
        let mut carried = KeyRecordDebt::after_target(true, true);
        assert!(!carried.settle(true));
        let mut clear = KeyRecordDebt::after_target(false, false);
        assert!(!clear.settle(true));
    }

    // -- The owed retransmission -----------------------------------------

    #[test]
    fn a_retry_debt_without_its_payload_is_forgotten() {
        for has_path in [false, true] {
            for asked in [false, true] {
                assert_eq!(owed_retry(false, has_path, asked), None);
                assert_eq!(
                    owed_retry(true, has_path, asked),
                    Some(retry_step(has_path, asked))
                );
            }
        }
    }

    // -- The position a report carries ------------------------------------

    #[test]
    fn a_sensor_fix_needs_both_latitude_and_longitude() {
        assert_eq!(Fix::from_degrees(None, Some(9.99), Some(1.2)), None);
        assert_eq!(Fix::from_degrees(Some(53.55), None, Some(1.2)), None);
        assert_eq!(
            Fix::from_degrees(Some(53.551086), Some(9.993682), Some(1.2)),
            Some(Fix {
                latitude_e6: 53_551_086,
                longitude_e6: 9_993_682,
                hdop_e2: Some(120),
            })
        );
    }

    #[test]
    fn a_sensor_fix_without_hdop_is_a_position_the_policy_will_not_report() {
        let fix = Fix::from_degrees(Some(53.55), Some(9.99), None);
        assert_eq!(fix.map(|f| f.hdop_e2), Some(None));
        assert_eq!(accuracy_m(None), None, "no HDOP, no sensor location");
        let mut policy = SendPolicy::new();
        policy.set_params(PolicyParams::TRACKER);
        assert_eq!(policy.reportable_position(fix), None);
    }

    #[test]
    fn a_sensor_location_is_hdop_times_five_metres() {
        assert_eq!(accuracy_m(Some(1.2)), Some(6.0));
        assert_eq!(accuracy_m(Some(0.0)), Some(0.0));
    }

    #[test]
    fn a_sensor_fix_counts_only_in_presence_state_fix() {
        let sensor = Fix::from_degrees(Some(53.55), Some(9.99), Some(1.0));
        assert_eq!(
            report_position(false, None, sensor),
            (None, PositionSource::Gnss)
        );
        assert_eq!(
            report_position(true, None, sensor),
            (sensor, PositionSource::Gnss)
        );
    }

    #[test]
    fn a_fixed_pin_replaces_the_sensor_without_gnss_or_with_it() {
        let pin = Some(Fix {
            latitude_e6: 1,
            longitude_e6: 2,
            hdop_e2: Some(crate::FIXED_POSITION_HDOP_E2),
        });
        let sensor = Fix::from_degrees(Some(53.55), Some(9.99), Some(1.0));
        for has_fix in [false, true] {
            assert_eq!(
                report_position(has_fix, pin, sensor),
                (pin, PositionSource::Fixed)
            );
        }
    }

    #[test]
    fn the_location_is_none_fixed_or_the_sensor() {
        use LocationSource as L;
        use PositionSource as P;
        assert_eq!(location_source(false, P::Fixed, true), L::None);
        assert_eq!(location_source(false, P::Gnss, false), L::None);
        assert_eq!(location_source(true, P::Fixed, true), L::Fixed);
        assert_eq!(location_source(true, P::Gnss, false), L::Sensor);
        assert_eq!(location_source(true, P::Gnss, true), L::Sensor);
        // A fixed source whose pin vanished falls back to the readings.
        assert_eq!(location_source(true, P::Fixed, false), L::Sensor);
    }

    #[test]
    fn the_cadence_records_a_position_only_if_the_report_carried_it() {
        let mut policy = SendPolicy::new();
        policy.set_params(PolicyParams::TRACKER);
        let good = Fix::from_degrees(Some(53.55), Some(9.99), Some(1.0));
        let poor = Fix::from_degrees(Some(53.55), Some(9.99), Some(99.0));
        assert_eq!(policy.reportable_position(good), good);
        assert_eq!(policy.reportable_position(poor), None);
        assert_eq!(policy.reportable_position(None), None);
    }

    // -- Withheld attempts -------------------------------------------------

    #[test]
    fn no_calendar_time_withholds_no_clock_and_books_nothing() {
        assert_eq!(report_time(None), Err(Withhold::NoClock));
        assert_eq!(report_time(Some(1_760_000_000)), Ok(1_760_000_000));
        let mut policy = SendPolicy::new();
        policy.note_withheld(5_000, Withhold::NoClock);
        assert!(policy.may_spend_airtime(5_000));
        assert_eq!(Withhold::NoClock.as_str(), "no-clock");
    }

    #[test]
    fn a_failure_after_the_announce_books_its_airtime_and_withholds() {
        for (reason, word) in [
            (Withhold::NoReadings, "no-readings"),
            (Withhold::NoOnAir, "no-on-air"),
            (Withhold::NoPath, "no-path"),
            (Withhold::IfaceOffline, "iface-offline"),
            (Withhold::SendFailed, "send-failed"),
        ] {
            let mut policy = SendPolicy::new();
            policy.set_params(PolicyParams::TRACKER);
            policy.note_withheld(5_000, reason);
            assert!(
                !policy.may_spend_airtime(5_000),
                "{word}: the announce is booked"
            );
            assert!(policy.may_spend_airtime(5_000 + PolicyParams::TRACKER.min_interval_ms));
            assert_eq!(reason.as_str(), word);
        }
    }

    #[test]
    fn no_usable_path_asks_and_withholds_naming_why() {
        assert_eq!(path_withhold(true, true), None);
        assert_eq!(path_withhold(true, false), None);
        assert_eq!(path_withhold(false, true), Some(Withhold::IfaceOffline));
        assert_eq!(path_withhold(false, false), Some(Withhold::NoPath));
    }

    // -- The flash write ---------------------------------------------------

    #[test]
    fn a_flash_that_fails_three_times_is_lost_after_three_pauses() {
        let mut save = FlashSave::new();
        let steps = [
            save.after_attempt(false),
            save.after_attempt(false),
            save.after_attempt(false),
        ];
        assert_eq!(
            steps,
            [
                SaveStep::Failed {
                    attempt: 1,
                    wait_ms: SAVE_RETRY_MS,
                    gave_up: false
                },
                SaveStep::Failed {
                    attempt: 2,
                    wait_ms: SAVE_RETRY_MS,
                    gave_up: false
                },
                SaveStep::Failed {
                    attempt: 3,
                    wait_ms: SAVE_RETRY_MS,
                    gave_up: true
                },
            ]
        );
    }

    #[test]
    fn a_flash_that_takes_the_second_write_is_written() {
        let mut save = FlashSave::new();
        assert!(matches!(
            save.after_attempt(false),
            SaveStep::Failed { gave_up: false, .. }
        ));
        assert_eq!(save.after_attempt(true), SaveStep::Written);
    }

    // -- Inbound hand-over -------------------------------------------------

    /// A depth-1 queue, as the board's `Channel<_, _, 1>` behaves.
    struct Slot(core::cell::Cell<Option<u8>>);

    impl Slot {
        fn deliver(&self, item: u8) -> bool {
            deliver_newest(
                item,
                |v| match self.0.get() {
                    Some(_) => Err(()),
                    None => {
                        self.0.set(Some(v));
                        Ok(())
                    }
                },
                || {
                    self.0.take();
                },
            )
        }
    }

    #[test]
    fn the_newest_inbound_frame_replaces_an_unread_one() {
        let slot = Slot(core::cell::Cell::new(None));
        assert!(slot.deliver(1));
        assert!(slot.deliver(2), "the unread 1 is displaced");
        assert_eq!(slot.0.take(), Some(2));
        assert!(slot.deliver(3));
        assert_eq!(slot.0.take(), Some(3));
    }

    #[test]
    fn a_queue_that_refuses_after_the_discard_reports_the_frame_not_taken() {
        let taken = deliver_newest(7u8, |_| Err::<(), ()>(()), || {});
        assert!(!taken);
    }
}
