//! Whether a peer's position is alive (1209): a position report is a state,
//! not a message, so it asks for no attention; what is worth seeing, when
//! one looks, is whether the reports still come at their pace or stopped.
//! Every screen words it from here: the terminal's fold line and list
//! entry, the window's, and the phone's through the facade's fields.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_lxmf::telemetry::ShareMark;

/// How many of the newest gaps between reports the cadence is learned
/// from: their median, so one late report does not move it.
pub const CADENCE_GAPS: usize = 5;
/// The shortest cadence: reports a few seconds apart (a burst, a pull of
/// a backlog) make no stream that should go stale in a minute.
pub const CADENCE_MIN_S: i64 = 60;
/// How many reports teach a cadence; fewer are judged by [`YOUNG_S`].
pub const CADENCE_REPORTS: usize = 3;
/// A stream without a cadence yet is live while its newest report is
/// younger than this.
pub const YOUNG_S: i64 = 15 * 60;
/// A position younger than this that is no longer live is recent.
pub const RECENT_S: i64 = 24 * 3600;

/// The state of a peer's position stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// Reports still come at their pace: the newest is younger than twice
    /// the cadence, or than [`YOUNG_S`] without one.
    Live,
    /// The stream stopped within the last [`RECENT_S`].
    Recent,
    /// Older than that, or no report at all.
    #[default]
    Stale,
}

impl State {
    /// The facade's word for it.
    pub fn word(self) -> &'static str {
        match self {
            State::Live => "live",
            State::Recent => "recent",
            State::Stale => "stale",
        }
    }
}

/// A peer's liveness as every screen shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Liveness {
    pub state: State,
    /// The learned report cadence in seconds, from [`CADENCE_REPORTS`]
    /// reports on.
    pub cadence_s: Option<i64>,
    /// When the newest report's fix was taken, unix seconds (the receive
    /// time where the sender's clock claims the future).
    pub last_fix_at: Option<i64>,
}

impl Liveness {
    /// The liveness of a stream of reports taken at `times`, in any order,
    /// judged at `now`. Without a clock a stream is at most recent: its age
    /// is unknown.
    pub fn of(times: &[i64], now: Option<i64>) -> Self {
        Self::of_marked(times, ShareMark::default(), now)
    }

    /// [`Liveness::of`], with the newest report's share mark (1273): a
    /// running share's promise `every` keeps the stream live for twice
    /// that, whatever the learned cadence says, and the share's end
    /// report makes it no longer live at all, only as old as it is.
    pub fn of_marked(times: &[i64], mark: ShareMark, now: Option<i64>) -> Self {
        let mut times = times.to_vec();
        times.sort_unstable();
        let Some(&newest) = times.last() else {
            return Self::default();
        };
        let cadence_s = (times.len() >= CADENCE_REPORTS).then(|| {
            let from = times.len().saturating_sub(CADENCE_GAPS + 1);
            let mut gaps: Vec<i64> = times[from..].windows(2).map(|w| w[1] - w[0]).collect();
            gaps.sort_unstable();
            // The upper median of an even count: the slower pace wins.
            gaps.get(gaps.len() / 2)
                .copied()
                .unwrap_or_default()
                .max(CADENCE_MIN_S)
        });
        let learned = cadence_s.map_or(YOUNG_S, |cadence| cadence.saturating_mul(2));
        let promised = mark
            .every
            .map_or(0, |every| i64::from(every).saturating_mul(2));
        let live_for = match mark.ended {
            true => 0,
            false => learned.max(promised),
        };
        let state = match now.map(|now| now.saturating_sub(newest)) {
            Some(age) if age < live_for => State::Live,
            Some(age) if age < RECENT_S => State::Recent,
            None => State::Recent,
            Some(_) => State::Stale,
        };
        Self {
            state,
            cadence_s,
            last_fix_at: Some(newest),
        }
    }

    /// The newest report's row says it, after its position: `live · every
    /// 10 min · 2 min ago`, or `last position 3 h ago` once the stream
    /// stopped; empty without a report.
    pub fn row_words(&self, now: Option<i64>) -> String {
        let Some(at) = self.last_fix_at else {
            return String::new();
        };
        let age = now.map(|now| ago(now.saturating_sub(at)));
        match (self.state, self.cadence_s, age) {
            (State::Live, Some(cadence), Some(age)) => {
                format!("live · {} · {age}", every(cadence))
            }
            (State::Live, None, Some(age)) => format!("live · {age}"),
            (_, _, Some(age)) => format!("last position {age}"),
            (_, _, None) => "last position".to_string(),
        }
    }

    /// The list's line for a conversation whose last row is a report:
    /// `live`, or the newest report's age, `3 h ago`.
    pub fn list_words(&self, now: Option<i64>) -> String {
        match (self.state, self.last_fix_at, now) {
            (State::Live, _, _) => "live".to_string(),
            (_, Some(at), Some(now)) => ago(now.saturating_sub(at)),
            _ => "position".to_string(),
        }
    }
}

/// An age in words: `now`, `2 min ago`, `3 h ago`, `4 d ago`.
pub fn ago(secs: i64) -> String {
    match secs {
        ..60 => "now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..172_800 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// A cadence in words: `every 10 min`, `every 2 h`.
pub fn every(cadence_s: i64) -> String {
    match cadence_s {
        ..3600 => format!("every {} min", (cadence_s + 30) / 60),
        _ => format!("every {} h", (cadence_s + 1800) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const T: i64 = 1_800_000_000;

    fn every_ten(n: i64) -> Vec<i64> {
        (0..n).map(|i| T + i * 600).collect()
    }

    #[test]
    fn six_reports_ten_minutes_apart_are_live_then_recent_then_stale() {
        let times = every_ten(6);
        let newest = T + 5 * 600;
        let live = Liveness::of(&times, Some(newest + 120));
        assert_eq!(live.state, State::Live);
        assert_eq!(live.cadence_s, Some(600));
        assert_eq!(live.last_fix_at, Some(newest));
        assert_eq!(
            live.row_words(Some(newest + 120)),
            "live · every 10 min · 2 min ago"
        );
        assert_eq!(live.list_words(Some(newest + 120)), "live");
        // Twice the cadence is the edge.
        assert_eq!(Liveness::of(&times, Some(newest + 1199)).state, State::Live);
        let recent = Liveness::of(&times, Some(newest + 25 * 60));
        assert_eq!(recent.state, State::Recent);
        assert_eq!(
            recent.row_words(Some(newest + 3 * 3600)),
            "last position 3 h ago"
        );
        assert_eq!(recent.list_words(Some(newest + 3 * 3600)), "3 h ago");
        assert_eq!(
            Liveness::of(&times, Some(newest + 25 * 3600)).state,
            State::Stale
        );
    }

    #[test]
    fn fewer_than_three_reports_are_live_for_fifteen_minutes() {
        let two = [T, T + 60];
        let young = Liveness::of(&two, Some(T + 60 + 14 * 60));
        assert_eq!((young.state, young.cadence_s), (State::Live, None));
        assert_eq!(young.row_words(Some(T + 60 + 14 * 60)), "live · 14 min ago");
        assert_eq!(
            Liveness::of(&two, Some(T + 60 + 15 * 60)).state,
            State::Recent
        );
    }

    #[test]
    fn the_cadence_is_the_median_of_the_last_five_gaps_and_at_least_a_minute() {
        // Old gaps of an hour fall out; one late report does not move it.
        let mut times = vec![T, T + 3600, T + 7200];
        let base = T + 7200;
        times.extend([base + 300, base + 600, base + 900, base + 2000, base + 2300]);
        assert_eq!(Liveness::of(&times, None).cadence_s, Some(300));
        // A burst is no one-second stream.
        let burst = [T, T + 1, T + 2, T + 3];
        assert_eq!(Liveness::of(&burst, None).cadence_s, Some(CADENCE_MIN_S));
        // Out of order is sorted first.
        let shuffled = [T + 1200, T, T + 600];
        assert_eq!(Liveness::of(&shuffled, None).cadence_s, Some(600));
    }

    /// A share's reports (1273): reports a minute apart at first teach a
    /// cadence of a minute, but the promise of 15 min keeps the stream
    /// live for half an hour once the phone rests; the end report makes
    /// it recent at once, with its age.
    #[test]
    fn a_share_stays_live_by_its_promise_and_its_end_report_ends_it() {
        let times = [T, T + 60, T + 120, T + 180];
        let newest = T + 180;
        let running = ShareMark {
            every: Some(900),
            ended: false,
        };
        let plain = Liveness::of(&times, Some(newest + 20 * 60));
        assert_eq!(plain.state, State::Recent, "the minute cadence alone");
        let promised = Liveness::of_marked(&times, running, Some(newest + 20 * 60));
        assert_eq!(promised.state, State::Live);
        assert_eq!(
            Liveness::of_marked(&times, running, Some(newest + 30 * 60)).state,
            State::Recent
        );
        let end = ShareMark {
            every: None,
            ended: true,
        };
        let ended = Liveness::of_marked(&times, end, Some(newest + 120));
        assert_eq!(ended.state, State::Recent);
        assert_eq!(
            ended.row_words(Some(newest + 120)),
            "last position 2 min ago"
        );
        assert_eq!(ended.list_words(Some(newest + 120)), "2 min ago");
    }

    #[test]
    fn no_report_is_stale_and_says_nothing() {
        let none = Liveness::of(&[], Some(T));
        assert_eq!(none, Liveness::default());
        assert_eq!(none.state, State::Stale);
        assert_eq!(none.row_words(Some(T)), "");
    }

    #[test]
    fn ages_and_cadences_in_words() {
        assert_eq!(ago(59), "now");
        assert_eq!(ago(120), "2 min ago");
        assert_eq!(ago(3 * 3600), "3 h ago");
        assert_eq!(ago(25 * 3600), "25 h ago");
        assert_eq!(ago(3 * 86_400), "3 d ago");
        assert_eq!(every(600), "every 10 min");
        assert_eq!(every(7200), "every 2 h");
    }
}
