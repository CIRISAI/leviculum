//! Sharing a position for a while (1273; Lew 2026-11-01: once, for a
//! certain time, or until stopped, and ending it must be simple).
//!
//! The position sheet offers five rows: "Now, once" sends one report as
//! before (1270), the others start a **share** for that chat: for 15
//! minutes, an hour, eight hours, or until the user stops it. The model
//! owns the share, its end time, its cadence and the last report it sent;
//! the platform only delivers fixes ([`super::UiEvent::OwnFix`]) and the
//! clock ([`super::UiEvent::Clock`]), and keeps itself awake while one
//! runs (the phone's foreground service).
//!
//! The cadence, after the board rule of `announce-cadence.md` (5 min
//! moving, 1 h still), shortened at rest so the receiver keeps seeing the
//! stream live (1209):
//!
//! - the first fix of a share goes at once;
//! - a fix more than [`MOVED_M`] from the last report goes, but never
//!   sooner than [`MIN_GAP_S`] after it;
//! - after a report for a move, one goes at least every
//!   [`MOVING_EVERY_S`] even without the next 25 m;
//! - at rest at most every [`STILL_EVERY_S`].
//!
//! Every report of a running share carries the [`ShareMark`] `every` =
//! [`STILL_EVERY_S`], the longest the next one waits, so the receiver
//! keeps it live for twice that whatever cadence it learned; the stop
//! sends one last report marked `ended`, and the receiver's state turns
//! from live to the age words.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use leviculum_lxmf::constants::FIELD_TELEMETRY;
use leviculum_lxmf::telemetry::{Location, ShareMark, Telemetry};
use leviculum_lxmf::Field;

use super::{UiEffect, UiModel};

/// A fix farther than this from the last report is a move, metres.
pub const MOVED_M: f64 = 25.0;
/// The shortest gap between two reports of a share, seconds: a moving car
/// delivers a fix a second.
pub const MIN_GAP_S: i64 = 60;
/// After a report for a move, the next goes after this at the latest,
/// seconds.
pub const MOVING_EVERY_S: i64 = 5 * 60;
/// At rest, a report this often, seconds; the longest any gap of a running
/// share gets, which every report promises the receiver.
pub const STILL_EVERY_S: i64 = 15 * 60;

/// How long a share runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Span {
    Minutes15,
    Hour1,
    Hours8,
    /// Until the user stops it: it survives a restart, as every share does.
    UntilStopped,
}

impl Span {
    /// The sheet's order.
    pub const ALL: [Span; 4] = [
        Span::Minutes15,
        Span::Hour1,
        Span::Hours8,
        Span::UntilStopped,
    ];

    /// Its length in seconds; none for the one that runs until stopped.
    pub fn secs(self) -> Option<i64> {
        match self {
            Span::Minutes15 => Some(15 * 60),
            Span::Hour1 => Some(3600),
            Span::Hours8 => Some(8 * 3600),
            Span::UntilStopped => None,
        }
    }

    /// The facade's and the store's word, and the sheet row's id tail.
    pub fn word(self) -> &'static str {
        match self {
            Span::Minutes15 => "15_min",
            Span::Hour1 => "1_h",
            Span::Hours8 => "8_h",
            Span::UntilStopped => "until_stopped",
        }
    }

    pub fn of_word(word: &str) -> Option<Self> {
        Span::ALL.into_iter().find(|span| span.word() == word)
    }
}

/// Why a report goes now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The share's first.
    First,
    /// The fix moved more than [`MOVED_M`].
    Moved,
    /// [`MOVING_EVERY_S`] after a move.
    Moving,
    /// [`STILL_EVERY_S`] at rest.
    Still,
}

/// The last report a share sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub fix: Location,
    /// When it went, unix seconds on the model's clock.
    pub at: i64,
    /// It went for a move: the next goes [`MOVING_EVERY_S`] after it.
    pub moved: bool,
}

/// A running share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Share {
    pub span: Span,
    /// Unix seconds.
    pub started_at: i64,
    /// When it ends, unix seconds; none until stopped.
    pub until: Option<i64>,
    pub last: Option<Report>,
}

impl Share {
    /// A share of `span` from `now`, nothing sent yet.
    pub fn start(span: Span, now: i64) -> Self {
        Share {
            span,
            started_at: now,
            until: span.secs().map(|secs| now.saturating_add(secs)),
            last: None,
        }
    }

    /// Whether its time is up at `now`.
    pub fn over(&self, now: i64) -> bool {
        self.until.is_some_and(|until| now >= until)
    }

    /// The seconds left at `now`; none for one that runs until stopped.
    pub fn left_s(&self, now: i64) -> Option<i64> {
        self.until.map(|until| until.saturating_sub(now).max(0))
    }

    /// Whether `fix` goes now, and why: the cadence of the module's head.
    pub fn due(&self, fix: &Location, now: i64) -> Option<Why> {
        let Some(last) = &self.last else {
            return Some(Why::First);
        };
        let gap = now.saturating_sub(last.at);
        if gap < MIN_GAP_S {
            return None;
        }
        if metres(&last.fix, fix) > MOVED_M {
            return Some(Why::Moved);
        }
        if last.moved && gap >= MOVING_EVERY_S {
            return Some(Why::Moving);
        }
        (gap >= STILL_EVERY_S).then_some(Why::Still)
    }
}

/// The distance between two fixes in metres: the equirectangular
/// approximation, exact enough at the 25 m this is asked about.
pub fn metres(a: &Location, b: &Location) -> f64 {
    const EARTH_M: f64 = 6_371_000.0;
    let rad = |e6: i32| f64::from(e6) / 1e6 * core::f64::consts::PI / 180.0;
    let mid = (rad(a.latitude_e6) + rad(b.latitude_e6)) / 2.0;
    let x = (rad(b.longitude_e6) - rad(a.longitude_e6)) * libm::cos(mid);
    let y = rad(b.latitude_e6) - rad(a.latitude_e6);
    libm::sqrt(x * x + y * y) * EARTH_M
}

/// The fields of one report of a share: the position report of 1209 with
/// its [`ShareMark`].
pub fn report_fields(location: Location, ended: bool) -> Vec<Field> {
    let telemetry = Telemetry {
        time: Some(location.last_update),
        location: Some(location),
        ..Default::default()
    };
    let mark = ShareMark {
        every: match ended {
            true => None,
            false => Some(STILL_EVERY_S as u32),
        },
        ended,
    };
    vec![(FIELD_TELEMETRY, telemetry.encode_field_value_marked(mark))]
}

/// The shares of a model and what the store has not learned of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shares {
    /// The running ones, by conversation.
    pub running: BTreeMap<[u8; 16], Share>,
    /// Conversations whose share started, sent or stopped since the store
    /// last wrote.
    pub(super) changed: alloc::collections::BTreeSet<[u8; 16]>,
}

impl<B> UiModel<B> {
    /// `peer`'s running share.
    pub fn share(&self, peer: &[u8; 16]) -> Option<&Share> {
        self.shares.running.get(peer)
    }

    /// The shares the store held at start: each picks up where it was, and
    /// one whose time ran out while the app was gone ends at the next
    /// clock with its end report.
    pub fn restore_shares(&mut self, shares: Vec<([u8; 16], Share)>) {
        for (peer, share) in shares {
            self.shares.running.insert(peer, share);
        }
    }

    /// [`super::UiEvent::StartShare`]: `peer`'s share of `span` replaces
    /// one that ran there, and goes at once when the fix is known. Without
    /// a clock nothing starts: the end time would be unknown.
    pub(super) fn start_share(&mut self, peer: [u8; 16], span: Span) -> Vec<UiEffect> {
        let Some(now) = self.now.map(|now| now.unix_secs) else {
            return Vec::new();
        };
        self.bar.sheet = None;
        self.conversation(peer);
        self.shares.running.insert(peer, Share::start(span, now));
        self.shares.changed.insert(peer);
        self.shares_due()
    }

    /// [`super::UiEvent::StopShare`]: the share ends with one last report
    /// marked as the end, at the newest fix (the last one sent where the
    /// shell has none now). Nothing to stop, nothing goes.
    pub(super) fn stop_share(&mut self, peer: [u8; 16]) -> Vec<UiEffect> {
        let Some(share) = self.shares.running.remove(&peer) else {
            return Vec::new();
        };
        self.shares.changed.insert(peer);
        let fix = self.bar.own_fix.or(share.last.map(|last| last.fix));
        match fix {
            Some(fix) => {
                let index = self.conversation(peer);
                self.send_fields(index, String::new(), report_fields(fix, true))
            }
            None => Vec::new(),
        }
    }

    /// After a fix or a clock: every share whose time is up ends, every
    /// other sends what its cadence says is due.
    pub(super) fn shares_due(&mut self) -> Vec<UiEffect> {
        let Some(now) = self.now.map(|now| now.unix_secs) else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        let over: Vec<[u8; 16]> = self
            .shares
            .running
            .iter()
            .filter(|(_, share)| share.over(now))
            .map(|(peer, _)| *peer)
            .collect();
        for peer in over {
            effects.extend(self.stop_share(peer));
        }
        let Some(fix) = self.bar.own_fix else {
            return effects;
        };
        let due: Vec<([u8; 16], Why)> = self
            .shares
            .running
            .iter()
            .filter_map(|(peer, share)| share.due(&fix, now).map(|why| (*peer, why)))
            .collect();
        for (peer, why) in due {
            if let Some(share) = self.shares.running.get_mut(&peer) {
                share.last = Some(Report {
                    fix,
                    at: now,
                    moved: why == Why::Moved,
                });
            }
            self.shares.changed.insert(peer);
            let index = self.conversation(peer);
            effects.extend(self.send_fields(index, String::new(), report_fields(fix, false)));
        }
        effects
    }

    /// The shares the store must learn: each changed one as it stands, the
    /// stopped ones as none.
    pub(super) fn take_share_writes(&mut self) -> Vec<([u8; 16], Option<Share>)> {
        core::mem::take(&mut self.shares.changed)
            .into_iter()
            .map(|peer| (peer, self.shares.running.get(&peer).copied()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::Field as Buffer;
    use crate::ui::{update, UiEvent, UiModel};
    use crate::when::WallTime;

    const PEER: [u8; 16] = [0xab; 16];
    const T: i64 = 1_793_500_000;

    fn fix(lat_e6: i32, at: i64) -> Location {
        Location {
            latitude_e6: lat_e6,
            longitude_e6: 11_256_000,
            altitude_e2: 0,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 500,
            last_update: at,
        }
    }

    /// One degree of latitude is 111 195 m: 27 e6 is 30 m.
    const THIRTY_M: i32 = 270;

    fn model() -> UiModel<Buffer> {
        let mut model = UiModel::new(&[PEER]);
        clock(&mut model, T);
        model
    }

    fn clock(model: &mut UiModel<Buffer>, at: i64) -> Vec<UiEffect> {
        update(
            model,
            UiEvent::Clock(WallTime {
                unix_secs: at,
                offset_secs: 0,
            }),
        )
    }

    /// The clock moves to `when`, then a fix taken there arrives: what
    /// both sent.
    fn at(model: &mut UiModel<Buffer>, when: i64, lat_e6: i32) -> Vec<UiEffect> {
        let mut effects = clock(model, when);
        effects.extend(update(model, UiEvent::OwnFix(Some(fix(lat_e6, when)))));
        effects
    }

    fn marks(effects: &[UiEffect]) -> Vec<ShareMark> {
        effects
            .iter()
            .filter_map(|e| match e {
                UiEffect::Send { to, body, fields } if *to == PEER && body.is_empty() => {
                    let (_, value) = fields.iter().find(|(k, _)| *k == FIELD_TELEMETRY)?;
                    let mut p = 0;
                    let blob = leviculum_lxmf::msgpack::read_bin(value, &mut p).ok()?;
                    Some(ShareMark::of_blob(blob))
                }
                _ => None,
            })
            .collect()
    }

    const RUNNING: ShareMark = ShareMark {
        every: Some(STILL_EVERY_S as u32),
        ended: false,
    };
    const END: ShareMark = ShareMark {
        every: None,
        ended: true,
    };

    #[test]
    fn the_spans_and_their_words() {
        assert_eq!(Span::Minutes15.secs(), Some(900));
        assert_eq!(Span::Hour1.secs(), Some(3600));
        assert_eq!(Span::Hours8.secs(), Some(8 * 3600));
        assert_eq!(Span::UntilStopped.secs(), None);
        for span in Span::ALL {
            assert_eq!(Span::of_word(span.word()), Some(span));
        }
        assert_eq!(Span::of_word("forever"), None);
    }

    #[test]
    fn thirty_metres_are_a_move_and_twenty_are_not() {
        let a = fix(43_773_100, T);
        assert!((metres(&a, &fix(43_773_100 + THIRTY_M, T)) - 30.0).abs() < 0.5);
        assert!(metres(&a, &fix(43_773_100 + 180, T)) < MOVED_M);
        assert_eq!(metres(&a, &a), 0.0);
    }

    /// The first fix goes at once; moved 30 m a minute later it goes
    /// again; moved 30 m within the minute it waits.
    #[test]
    fn a_share_sends_its_first_fix_and_a_move_of_thirty_metres() {
        let mut model = model();
        update(&mut model, UiEvent::OwnFix(Some(fix(43_773_100, T))));
        let started = update(
            &mut model,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::Hour1,
            },
        );
        assert_eq!(marks(&started), [RUNNING], "the first at once");
        let share = model.share(&PEER).copied().expect("running");
        assert_eq!(share.until, Some(T + 3600));
        assert!(
            at(&mut model, T + 30, 43_773_100 + THIRTY_M).is_empty(),
            "not within the minute"
        );
        let moved = at(&mut model, T + 60, 43_773_100 + THIRTY_M);
        assert_eq!(marks(&moved), [RUNNING], "moved 30 m: it goes");
        assert!(model
            .share(&PEER)
            .and_then(|s| s.last)
            .is_some_and(|l| l.moved));
        // Five minutes after the move, the moving heartbeat, though it
        // stood since.
        assert!(at(&mut model, T + 60 + 299, 43_773_100 + THIRTY_M).is_empty());
        let beat = at(&mut model, T + 60 + 300, 43_773_100 + THIRTY_M);
        assert_eq!(marks(&beat), [RUNNING]);
    }

    /// At rest for twenty minutes, fixes every minute: one report at the
    /// start and one at fifteen minutes, no more.
    #[test]
    fn still_for_twenty_minutes_sends_once() {
        let mut model = model();
        update(&mut model, UiEvent::OwnFix(Some(fix(43_773_100, T))));
        let started = update(
            &mut model,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::UntilStopped,
            },
        );
        assert_eq!(marks(&started).len(), 1);
        let mut sent = Vec::new();
        for minute in 1..=20 {
            // A jitter of 5 m is no move.
            let lat = 43_773_100 + (minute % 2) * 45;
            sent.extend(marks(&at(&mut model, T + minute as i64 * 60, lat)));
        }
        assert_eq!(sent, [RUNNING], "one in twenty minutes at rest");
        assert_eq!(
            model.share(&PEER).and_then(|s| s.last).map(|l| l.at),
            Some(T + STILL_EVERY_S)
        );
    }

    /// A clock alone, with no new fix, sends the still report from the
    /// newest fix: a phone at rest may deliver none.
    #[test]
    fn the_clock_alone_sends_the_still_report() {
        let mut model = model();
        update(&mut model, UiEvent::OwnFix(Some(fix(43_773_100, T))));
        update(
            &mut model,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::UntilStopped,
            },
        );
        assert!(clock(&mut model, T + STILL_EVERY_S - 1).is_empty());
        assert_eq!(marks(&clock(&mut model, T + STILL_EVERY_S)), [RUNNING]);
    }

    /// The end time stops it with the end report, and nothing goes after.
    #[test]
    fn the_end_time_stops_the_share_with_the_end_report() {
        let mut model = model();
        update(&mut model, UiEvent::OwnFix(Some(fix(43_773_100, T))));
        update(
            &mut model,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::Minutes15,
            },
        );
        assert_eq!(model.share(&PEER).and_then(|s| s.left_s(T + 60)), Some(840));
        assert!(clock(&mut model, T + 899).is_empty());
        let ended = clock(&mut model, T + 900);
        assert_eq!(marks(&ended), [END], "only the end, no still report");
        assert_eq!(model.share(&PEER), None);
        assert!(at(&mut model, T + 2000, 43_773_100 + 10 * THIRTY_M).is_empty());
    }

    /// Stop sends one report flagged as the end; without any fix ever the
    /// share just ends.
    #[test]
    fn stop_sends_the_end_report_and_a_share_without_a_fix_just_ends() {
        let mut model = model();
        update(&mut model, UiEvent::OwnFix(Some(fix(43_773_100, T))));
        update(
            &mut model,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::UntilStopped,
            },
        );
        let stopped = update(&mut model, UiEvent::StopShare { peer: PEER });
        assert_eq!(marks(&stopped), [END]);
        assert!(update(&mut model, UiEvent::StopShare { peer: PEER }).is_empty());

        let mut blind = self::model();
        let started = update(
            &mut blind,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::Hour1,
            },
        );
        assert!(started.is_empty(), "no fix yet: it waits for one");
        assert!(blind.share(&PEER).is_some());
        assert_eq!(marks(&at(&mut blind, T + 5, 43_773_100)), [RUNNING]);
        assert!(update(&mut blind, UiEvent::StopShare { peer: PEER })
            .iter()
            .any(|e| matches!(e, UiEffect::Send { .. })));
    }

    /// The store learns each change once: the share as it stands, the
    /// stopped one as none; and a restored share whose time ran out ends at
    /// the first clock.
    #[test]
    fn the_store_learns_the_shares_and_a_restored_one_ends_when_due() {
        let mut model = model();
        update(
            &mut model,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::Minutes15,
            },
        );
        let writes = model.take_share_writes();
        assert_eq!(writes, [(PEER, Some(Share::start(Span::Minutes15, T)))]);
        assert!(model.take_share_writes().is_empty());
        update(&mut model, UiEvent::StopShare { peer: PEER });
        assert_eq!(model.take_share_writes(), [(PEER, None)]);

        let mut later: UiModel<Buffer> = UiModel::new(&[PEER]);
        let mut share = Share::start(Span::Minutes15, T);
        share.last = Some(Report {
            fix: fix(43_773_100, T),
            at: T,
            moved: false,
        });
        later.restore_shares(vec![(PEER, share)]);
        let ended = clock(&mut later, T + 3600);
        assert_eq!(marks(&ended), [END], "the last fix it sent, as the end");
        assert_eq!(later.share(&PEER), None);
    }

    /// Without a clock nothing starts.
    #[test]
    fn no_clock_no_share() {
        let mut model: UiModel<Buffer> = UiModel::new(&[PEER]);
        assert!(update(
            &mut model,
            UiEvent::StartShare {
                peer: PEER,
                span: Span::Hour1,
            },
        )
        .is_empty());
        assert_eq!(model.share(&PEER), None);
    }
}
