//! POSIX TZ strings: the footer of a TZif file (RFC 8536, section 3.3)
//! and the board's clock rules.
//!
//! `std offset [dst [offset] [,start[/time],end[/time]]]`. A name is three
//! or more letters, or anything of letters, digits, `+` and `-` between
//! `<` and `>` (the brackets are not part of it). An offset is
//! `[+-]hh[:mm[:ss]]` and counts west of Greenwich, so `CET-1` is one
//! hour east of UTC; the DST offset defaults to one hour east of the
//! standard one. A date is `Jn` (1 to 365, February 29 never counted),
//! `n` (0 to 365, counted) or `Mm.w.d` (day `d`, 0 Sunday, of week `w` of
//! month `m`, 5 the last such day). A time is the local time of the
//! transition, the standard time's for the start and the DST's for the
//! end, default 02:00; RFC 8536's extension allows it to be negative and
//! up to 167 hours, so `M3.4.4/50` is two in the morning on the Saturday
//! after the fourth Thursday of March. DST without a rule gets the US
//! rule, `M3.2.0,M11.1.0`, as glibc gives it.

/// The local time a zone keeps at an instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offset<'a> {
    /// Seconds east of UTC: local time is UTC plus this.
    pub utc_offset: i32,
    /// Daylight saving time.
    pub dst: bool,
    /// The abbreviation, `CEST`, `+0545`.
    pub abbr: &'a str,
}

/// A transition's day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Date {
    /// `Jn`: day 1 to 365, February 29 never counted.
    NoLeap(u16),
    /// `n`: day 0 to 365, February 29 counted.
    Leap(u16),
    /// `Mm.w.d`: weekday `day` (0 Sunday) of week `week` (5 the last) of
    /// `month`.
    Month { month: u8, week: u8, day: u8 },
}

/// A transition: its day and its local time in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub date: Date,
    pub time: i32,
}

/// The daylight saving half of a TZ string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dst<'a> {
    pub abbr: &'a str,
    /// Seconds east of UTC.
    pub utc_offset: i32,
    pub start: Rule,
    pub end: Rule,
}

/// A parsed TZ string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PosixTz<'a> {
    pub abbr: &'a str,
    /// Seconds east of UTC.
    pub utc_offset: i32,
    pub dst: Option<Dst<'a>>,
}

/// The US rule, for DST named without one.
const DEFAULT_START: Rule = Rule {
    date: Date::Month {
        month: 3,
        week: 2,
        day: 0,
    },
    time: 7200,
};
const DEFAULT_END: Rule = Rule {
    date: Date::Month {
        month: 11,
        week: 1,
        day: 0,
    },
    time: 7200,
};

struct Cursor<'a> {
    s: &'a str,
    at: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.at).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.at += 1;
            return true;
        }
        false
    }

    fn name(&mut self) -> Option<&'a str> {
        let start = self.at;
        if self.eat(b'<') {
            while let Some(c) = self.peek() {
                if c == b'>' {
                    break;
                }
                if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'-') {
                    return None;
                }
                self.at += 1;
            }
            let name = self.s.get(start + 1..self.at)?;
            if !self.eat(b'>') || name.len() < 3 {
                return None;
            }
            return Some(name);
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.at += 1;
        }
        let name = self.s.get(start..self.at)?;
        (name.len() >= 3).then_some(name)
    }

    fn number(&mut self, max: u32) -> Option<u32> {
        let start = self.at;
        let mut v = 0u32;
        while let Some(c) = self.peek().filter(u8::is_ascii_digit) {
            v = v.checked_mul(10)?.checked_add(u32::from(c - b'0'))?;
            self.at += 1;
        }
        (self.at > start && v <= max).then_some(v)
    }

    /// `[+-]hh[:mm[:ss]]` in seconds, hours up to `max_hours`.
    fn hms(&mut self, max_hours: u32) -> Option<i32> {
        let negative = match self.peek() {
            Some(b'-') => {
                self.at += 1;
                true
            }
            Some(b'+') => {
                self.at += 1;
                false
            }
            _ => false,
        };
        let mut secs = self.number(max_hours)? * 3600;
        if self.eat(b':') {
            secs += self.number(59)? * 60;
            if self.eat(b':') {
                secs += self.number(59)?;
            }
        }
        let secs = i32::try_from(secs).ok()?;
        Some(if negative { -secs } else { secs })
    }

    fn rule(&mut self) -> Option<Rule> {
        let date = match self.peek()? {
            b'J' => {
                self.at += 1;
                let n = self.number(365)?;
                if n == 0 {
                    return None;
                }
                Date::NoLeap(n as u16)
            }
            b'M' => {
                self.at += 1;
                let month = self.number(12)? as u8;
                if month == 0 || !self.eat(b'.') {
                    return None;
                }
                let week = self.number(5)? as u8;
                if week == 0 || !self.eat(b'.') {
                    return None;
                }
                let day = self.number(6)? as u8;
                Date::Month { month, week, day }
            }
            _ => Date::Leap(self.number(365)? as u16),
        };
        let time = match self.eat(b'/') {
            true => self.hms(167)?,
            false => 7200,
        };
        Some(Rule { date, time })
    }

    fn at_offset(&self) -> bool {
        matches!(self.peek(), Some(b'+' | b'-' | b'0'..=b'9'))
    }
}

impl<'a> PosixTz<'a> {
    /// Parse `s`, all of it; `None` for anything that is not a TZ string.
    pub fn parse(s: &'a str) -> Option<Self> {
        let mut c = Cursor { s, at: 0 };
        let abbr = c.name()?;
        // Hours 0 to 24 by POSIX; RFC 8536 lets a footer go to 167, and
        // no zone has needed more than 25.
        let utc_offset = -c.hms(167)?;
        if c.peek().is_none() {
            return Some(Self {
                abbr,
                utc_offset,
                dst: None,
            });
        }
        let dst_abbr = c.name()?;
        let dst_offset = match c.at_offset() {
            true => -c.hms(167)?,
            false => utc_offset + 3600,
        };
        let (start, end) = match c.eat(b',') {
            true => {
                let start = c.rule()?;
                if !c.eat(b',') {
                    return None;
                }
                (start, c.rule()?)
            }
            false => (DEFAULT_START, DEFAULT_END),
        };
        if c.peek().is_some() {
            return None;
        }
        Some(Self {
            abbr,
            utc_offset,
            dst: Some(Dst {
                abbr: dst_abbr,
                utc_offset: dst_offset,
                start,
                end,
            }),
        })
    }

    /// The local time at `unix` seconds since the epoch.
    pub fn offset_at(&self, unix: i64) -> Offset<'a> {
        let standard = Offset {
            utc_offset: self.utc_offset,
            dst: false,
            abbr: self.abbr,
        };
        let Some(dst) = self.dst else {
            return standard;
        };
        let daylight = Offset {
            utc_offset: dst.utc_offset,
            dst: true,
            abbr: dst.abbr,
        };
        // The latest transition at or before `unix` among those of the
        // year around it, the later year winning a tie: a zone in DST all
        // year ends it at the instant the next year starts it.
        let year = civil_year((unix + i64::from(self.utc_offset)).div_euclid(86_400));
        let mut best: Option<(i64, i64, bool)> = None;
        for y in year - 1..=year + 1 {
            let start = day_of(y, dst.start.date) * 86_400 + i64::from(dst.start.time)
                - i64::from(self.utc_offset);
            let end = day_of(y, dst.end.date) * 86_400 + i64::from(dst.end.time)
                - i64::from(dst.utc_offset);
            for (at, on) in [(start, true), (end, false)] {
                if at <= unix && best.is_none_or(|(t, by, _)| (at, y) >= (t, by)) {
                    best = Some((at, y, on));
                }
            }
        }
        match best {
            Some((_, _, true)) => daylight,
            _ => standard,
        }
    }
}

/// The local time `tz` gives at `unix`; `None` for a string that is not a
/// TZ string.
pub fn offset_at(tz: &str, unix: i64) -> Option<Offset<'_>> {
    Some(PosixTz::parse(tz)?.offset_at(unix))
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn month_len(y: i64, m: u8) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's
/// `days_from_civil`).
pub(crate) fn days_from_civil(y: i64, m: u8, d: u8) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil year of a day since 1970-01-01.
fn civil_year(days: i64) -> i64 {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    yoe + era * 400 + i64::from(mp >= 10)
}

/// The day since 1970-01-01 a rule's date falls on in year `y`.
fn day_of(y: i64, date: Date) -> i64 {
    let jan1 = days_from_civil(y, 1, 1);
    match date {
        Date::NoLeap(n) => {
            let n = i64::from(n);
            jan1 + n - 1 + i64::from(is_leap(y) && n >= 60)
        }
        Date::Leap(n) => jan1 + i64::from(n),
        Date::Month { month, week, day } => {
            let first = days_from_civil(y, month, 1);
            // 1970-01-01 was a Thursday.
            let weekday = (first + 4).rem_euclid(7);
            let mut d = (i64::from(day) - weekday).rem_euclid(7) + 7 * (i64::from(week) - 1);
            while d >= month_len(y, month) {
                d -= 7;
            }
            first + d
        }
    }
}
