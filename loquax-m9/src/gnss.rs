//! The M9's own position (1477): the Airoha AG3352's NMEA read into a fix
//! the model takes as the own one ([`loquax::ui::UiEvent::OwnFix`]), the
//! words the map shows about the sky ([`Sky`]), the `[GNSS]` line, and
//! when the receiver is powered ([`Duty`]).
//!
//! Pure and `no_std`, so the host tests every rule over recorded and
//! written sentences; the firmware (`leviculum-esp/src/gnss_uart.rs` and
//! the M9 binary) moves bytes, sets the enable line and sends the init
//! frames of `leviculum-gnss-init`'s `AirohaInit::ag3352`.
//!
//! The parse is integer-only: a coordinate's `ddmm.mmmmm` becomes
//! microdegrees without a float (the nRF family's float parse cost and
//! its rounding were Codeberg #386). Only what the board uses is read:
//! GGA (fix quality, satellites in use, HDOP, altitude, position), RMC
//! (status, position, speed, course, date and time) and GSV (satellites
//! in view, per constellation), each checked against its checksum.
//!
//! What the module holds across sentences ([`Receiver`]): the last fix,
//! kept after the sky is lost, which is the "last known position" the
//! map centres on; and the satellites in view while it searches.

use alloc::string::String;

/// The fix as the model takes it, for a caller without the LXMF crate.
pub use leviculum_lxmf::telemetry::Location;

/// How many bytes an NMEA sentence has at most: 82 by the standard; the
/// AG3352's proprietary answers are shorter.
pub const LINE_MAX: usize = 96;

/// The horizontal error a unit of HDOP stands for, in metres: the model
/// the nRF reporter states as one (`leviculum-nrf/telemetry-policy`,
/// `HORIZONTAL_UERE_M`), not a measurement of this antenna.
pub const UERE_M: u32 = 5;

/// One NMEA sentence the board reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nmea {
    /// GGA: fix quality (0 none), satellites in use, HDOP in tenths,
    /// altitude in centimetres, position in microdegrees. The altitude is
    /// the height above the WGS84 ellipsoid (1684): the sentence's height
    /// above mean sea level plus its geoid separation, the height Android
    /// and Sideband put on the wire, which every reader turns into a
    /// height above sea level by one geoid
    /// ([`leviculum_map::geoid`]). A sentence without a separation gives
    /// its height as it is.
    Gga {
        quality: u8,
        sats: u8,
        hdop_e1: Option<u16>,
        alt_e2: Option<i32>,
        at: Option<(i32, i32)>,
    },
    /// RMC: status A, position, speed in cm/s, course in hundredths of a
    /// degree, UTC as unix seconds.
    Rmc {
        valid: bool,
        at: Option<(i32, i32)>,
        speed_e2: Option<u32>,
        course_e2: Option<i32>,
        unix: Option<i64>,
    },
    /// GSV: a constellation's satellites in view (the talker's two
    /// letters, the count).
    Gsv { talker: [u8; 2], in_view: u8 },
    /// `$PAIR001,<command>,<result>`: the receiver's answer to a `$PAIR`
    /// command. The M9 recorded `$PAIR001,002,1` then `$PAIR001,002,0`
    /// at power-up (window 22, 2026-11-09): 1 while the command runs, 0
    /// once it has.
    Ack { command: u16, result: u8 },
    /// Any other `$PAIR` sentence: the version answer (`$PAIR021`), the
    /// system message (`$PAIR011`), the aiding request (`$PAIR010`).
    Pair { command: u16 },
    /// A standard sentence with a good checksum that the board does not
    /// read: GLL, GSA, VTG, ZDA, which the init switches off.
    Unread { talker: [u8; 2], kind: [u8; 3] },
}

/// Joins the receiver's bytes into lines, `$` to the line end; a line
/// longer than [`LINE_MAX`] is dropped whole.
#[derive(Clone, Debug)]
pub struct Lines {
    buf: [u8; LINE_MAX],
    len: usize,
    over: bool,
}

impl Default for Lines {
    fn default() -> Self {
        Self::new()
    }
}

impl Lines {
    pub const fn new() -> Self {
        Self {
            buf: [0; LINE_MAX],
            len: 0,
            over: false,
        }
    }

    /// Feed `bytes`; `line` gets every whole line, without its line end.
    pub fn push(&mut self, bytes: &[u8], mut line: impl FnMut(&[u8])) {
        for &b in bytes {
            match b {
                b'$' => {
                    self.len = 0;
                    self.over = false;
                    self.buf[0] = b;
                    self.len = 1;
                }
                b'\r' | b'\n' => {
                    if self.len > 0 && !self.over {
                        line(&self.buf[..self.len]);
                    }
                    self.len = 0;
                    self.over = false;
                }
                _ if self.len == 0 => {}
                _ if self.len >= LINE_MAX => self.over = true,
                _ => {
                    self.buf[self.len] = b;
                    self.len += 1;
                }
            }
        }
    }
}

/// The sentence `line` holds (`$..*hh`, no line end), or `None` for a
/// bad checksum or a head that is neither a standard sentence nor a
/// `$PAIR` one.
pub fn parse(line: &[u8]) -> Option<Nmea> {
    let body = checked(line)?;
    let mut fields = body.split(|&b| b == b',');
    let head = fields.next()?;
    let f: alloc::vec::Vec<&[u8]> = fields.collect();
    let get = |i: usize| f.get(i).copied().unwrap_or(b"");
    if let Some(id) = head.strip_prefix(b"PAIR") {
        if id.len() != 3 {
            return None;
        }
        let command = u16::try_from(uint(id)?).ok()?;
        if command != 1 {
            return Some(Nmea::Pair { command });
        }
        return Some(Nmea::Ack {
            command: u16::try_from(uint(get(0))?).ok()?,
            result: u8::try_from(uint(get(1))?).ok()?,
        });
    }
    if head.len() != 5 || !head.iter().all(u8::is_ascii_uppercase) {
        return None;
    }
    let talker = [head[0], head[1]];
    match &head[2..] {
        b"GGA" => Some(Nmea::Gga {
            quality: uint(get(5)).unwrap_or(0) as u8,
            sats: uint(get(6)).unwrap_or(0).min(255) as u8,
            hdop_e1: fixed(get(7), 1).and_then(|v| u16::try_from(v).ok()),
            alt_e2: fixed(get(8), 2)
                .map(|msl| msl + fixed(get(10), 2).unwrap_or(0))
                .and_then(|v| i32::try_from(v).ok()),
            at: position(get(1), get(2), get(3), get(4)),
        }),
        b"RMC" => Some(Nmea::Rmc {
            valid: get(1) == b"A",
            at: position(get(2), get(3), get(4), get(5)),
            // Knots to cm/s: 1 kn = 1852/3600 m/s.
            speed_e2: fixed(get(6), 2).and_then(|kn_e2| u32::try_from(kn_e2 * 1852 / 3600).ok()),
            course_e2: fixed(get(7), 2).and_then(|v| i32::try_from(v).ok()),
            unix: unix(get(0), get(8)),
        }),
        b"GSV" => Some(Nmea::Gsv {
            talker,
            in_view: uint(get(2)).unwrap_or(0).min(255) as u8,
        }),
        _ => Some(Nmea::Unread {
            talker,
            kind: [head[2], head[3], head[4]],
        }),
    }
}

/// The body between `$` and `*` when the two hex digits after `*` are its
/// XOR checksum.
fn checked(line: &[u8]) -> Option<&[u8]> {
    let line = line.strip_prefix(b"$")?;
    let star = line.iter().position(|&b| b == b'*')?;
    let (body, tail) = line.split_at(star);
    let hex = tail.get(1..3)?;
    let want = u8::from_str_radix(core::str::from_utf8(hex).ok()?, 16).ok()?;
    let sum = body.iter().fold(0u8, |a, &b| a ^ b);
    (sum == want).then_some(body)
}

/// An unsigned decimal field.
fn uint(field: &[u8]) -> Option<u64> {
    if field.is_empty() || field.len() > 18 {
        return None;
    }
    field.iter().try_fold(0u64, |a, &b| {
        b.is_ascii_digit().then(|| a * 10 + u64::from(b - b'0'))
    })
}

/// A signed decimal field as an integer of `places` decimals, the digits
/// past them dropped (`12.345` at 2 is 1234).
fn fixed(field: &[u8], places: u32) -> Option<i64> {
    let (neg, digits) = match field.first() {
        Some(b'-') => (true, &field[1..]),
        _ => (false, field),
    };
    let (whole, frac) = match digits.iter().position(|&b| b == b'.') {
        Some(dot) => (&digits[..dot], &digits[dot + 1..]),
        None => (digits, &b""[..]),
    };
    let whole = match whole.is_empty() {
        true if !frac.is_empty() => 0,
        _ => uint(whole)?,
    };
    let mut v = i64::try_from(whole).ok()?;
    for i in 0..places as usize {
        let d = match frac.get(i) {
            Some(b) if b.is_ascii_digit() => i64::from(b - b'0'),
            Some(_) => return None,
            None => 0,
        };
        v = v.checked_mul(10)?.checked_add(d)?;
    }
    Some(if neg { -v } else { v })
}

/// `ddmm.mmmmm` (latitude) or `dddmm.mmmmm` (longitude) with its
/// hemisphere as microdegrees, the minutes' digits past the fifth
/// dropped.
fn degrees(field: &[u8], hemisphere: &[u8], negative: u8) -> Option<i32> {
    let dot = field.iter().position(|&b| b == b'.').unwrap_or(field.len());
    if dot < 3 {
        return None;
    }
    let deg = uint(&field[..dot - 2])?;
    // Minutes in hundred-thousandths.
    let min_e5 = fixed(&field[dot - 2..], 5)?;
    if deg > 180 || !(0..6_000_000).contains(&min_e5) {
        return None;
    }
    // e5 minutes to e6 degrees: * 10 / 60.
    let e6 = deg as i64 * 1_000_000 + min_e5 * 10 / 60;
    let e6 = i32::try_from(e6).ok()?;
    match hemisphere {
        [h] if *h == negative => Some(-e6),
        [_] => Some(e6),
        _ => None,
    }
}

/// A sentence's position, both halves or none.
fn position(lat: &[u8], ns: &[u8], lon: &[u8], ew: &[u8]) -> Option<(i32, i32)> {
    let lat = degrees(lat, ns, b'S')?;
    let lon = degrees(lon, ew, b'W')?;
    (lat.abs() <= 90_000_000 && lon.abs() <= 180_000_000).then_some((lat, lon))
}

/// RMC's `hhmmss.ss` and `ddmmyy` as unix seconds; two-digit years are
/// 2000 to 2099.
fn unix(time: &[u8], date: &[u8]) -> Option<i64> {
    let two = |s: &[u8], i: usize| uint(s.get(i..i + 2)?);
    let (h, m, s) = (two(time, 0)?, two(time, 2)?, two(time, 4)?);
    let (d, mo, y) = (two(date, 0)?, two(date, 2)?, two(date, 4)?);
    if h > 23 || m > 59 || s > 60 || !(1..=31).contains(&d) || !(1..=12).contains(&mo) {
        return None;
    }
    let days = days_from_civil(2000 + y as i64, mo as i64, d as i64);
    Some(days * 86_400 + (h * 3600 + m * 60 + s) as i64)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// What the receiver says about the sky, for the map's corner and the
/// `[GNSS]` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sky {
    /// Not powered.
    #[default]
    Off,
    /// Powered, no fix yet: the satellites in view.
    Searching { in_view: u8 },
    /// A fix, `age_s` seconds old.
    Fix { age_s: u32 },
}

impl Sky {
    /// The `state=` word of the `[GNSS]` line.
    pub fn word(&self) -> &'static str {
        match self {
            Sky::Off => "off",
            Sky::Searching { .. } => "searching",
            Sky::Fix { .. } => "fix",
        }
    }
}

/// How the GSV constellations are kept: the talkers the AG3352 sends
/// (GP, GL, GA, GB, GQ, GI).
const TALKERS: usize = 6;

/// The receiver as the sentences tell it.
#[derive(Clone, Debug, Default)]
pub struct Receiver {
    /// Powered since this many ms, `None` while off.
    on_since: Option<u64>,
    /// Satellites in view per talker.
    in_view: [([u8; 2], u8); TALKERS],
    /// From the latest GGA.
    sats: u8,
    hdop_e1: Option<u16>,
    alt_e2: Option<i32>,
    quality: u8,
    /// The latest fix and when it came (board ms); kept when the sky is
    /// lost: the last known position.
    fix: Option<(Location, u64)>,
    /// Whether the latest RMC said A.
    valid: bool,
    /// The sentences read: GGA, GSV and RMC with a good checksum. The
    /// init's gate counts these.
    pub sentences: u32,
    /// The receiver's `$PAIR` sentences: answers to our commands and its
    /// own power-up messages. Window 22 recorded seven before the first
    /// command went out.
    pub replies: u32,
    /// Standard sentences with a good checksum the board does not read.
    pub unread: u32,
    /// Lines with a bad checksum or a head that is no sentence.
    pub rejected: u32,
}

impl Receiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// The module was powered at `now_ms` (`true`) or cut (`false`):
    /// what it said of the sky goes, the last fix stays.
    pub fn power(&mut self, on: bool, now_ms: u64) {
        self.on_since = on.then_some(now_ms);
        self.in_view = Default::default();
        self.sats = 0;
        self.quality = 0;
        self.valid = false;
    }

    /// Whether it is powered.
    pub fn on(&self) -> bool {
        self.on_since.is_some()
    }

    /// Take `line` at `now_ms`; a new fix when it made one.
    pub fn line(&mut self, line: &[u8], now_ms: u64) -> Option<Location> {
        let Some(nmea) = parse(line) else {
            self.rejected = self.rejected.saturating_add(1);
            return None;
        };
        match nmea {
            Nmea::Ack { .. } | Nmea::Pair { .. } => {
                self.replies = self.replies.saturating_add(1);
                return None;
            }
            Nmea::Unread { .. } => {
                self.unread = self.unread.saturating_add(1);
                return None;
            }
            _ => self.sentences = self.sentences.saturating_add(1),
        }
        match nmea {
            Nmea::Gga {
                quality,
                sats,
                hdop_e1,
                alt_e2,
                ..
            } => {
                self.quality = quality;
                self.sats = sats;
                if quality > 0 {
                    self.hdop_e1 = hdop_e1;
                    self.alt_e2 = alt_e2;
                }
                None
            }
            Nmea::Gsv { talker, in_view } => {
                let slot = self
                    .in_view
                    .iter()
                    .position(|(t, _)| *t == talker)
                    .or_else(|| self.in_view.iter().position(|(t, _)| *t == [0, 0]));
                if let Some(i) = slot {
                    self.in_view[i] = (talker, in_view);
                }
                None
            }
            Nmea::Rmc {
                valid,
                at,
                speed_e2,
                course_e2,
                unix,
            } => {
                self.valid = valid;
                let ((lat, lon), unix) = match (valid, at, unix) {
                    (true, Some(at), Some(unix)) => (at, unix),
                    _ => return None,
                };
                let accuracy = self
                    .hdop_e1
                    .map(|h| u32::from(h) * UERE_M * 10)
                    .unwrap_or(0);
                let location = Location::saturating(
                    i64::from(lat),
                    i64::from(lon),
                    i64::from(self.alt_e2.unwrap_or(0)),
                    i64::from(speed_e2.unwrap_or(0)),
                    i64::from(course_e2.unwrap_or(0)),
                    i64::from(accuracy),
                    unix,
                );
                self.fix = Some((location, now_ms));
                Some(location)
            }
            Nmea::Ack { .. } | Nmea::Pair { .. } | Nmea::Unread { .. } => None,
        }
    }

    /// The satellites in view, all constellations.
    pub fn in_view(&self) -> u8 {
        self.in_view
            .iter()
            .map(|(_, n)| u32::from(*n))
            .sum::<u32>()
            .min(255) as u8
    }

    /// The satellites the latest GGA used.
    pub fn sats(&self) -> u8 {
        self.sats
    }

    /// The latest HDOP in tenths.
    pub fn hdop_e1(&self) -> Option<u16> {
        self.hdop_e1
    }

    /// The last fix and when it came, kept after the sky is lost.
    pub fn last(&self) -> Option<(Location, u64)> {
        self.fix
    }

    /// The sky at `now_ms`: off; a fix while the receiver holds one; else
    /// searching.
    pub fn sky(&self, now_ms: u64) -> Sky {
        if !self.on() {
            return Sky::Off;
        }
        match self.fix {
            Some((_, at)) if self.valid => Sky::Fix {
                age_s: (now_ms.saturating_sub(at) / 1000).min(u64::from(u32::MAX)) as u32,
            },
            _ => Sky::Searching {
                in_view: self.in_view(),
            },
        }
    }

    /// The `[GNSS]` line's body at `now_ms`: `state= sats= in_view= hdop=
    /// age_s=`, `age_s=none` before the first fix and the last fix's age
    /// after it, whether the receiver still holds one or not.
    pub fn line_words(&self, now_ms: u64) -> String {
        let hdop = match self.hdop_e1 {
            Some(h) => alloc::format!("{}.{}", h / 10, h % 10),
            None => String::from("none"),
        };
        let age = match self.fix {
            Some((_, at)) => alloc::format!("{}", now_ms.saturating_sub(at) / 1000),
            None => String::from("none"),
        };
        alloc::format!(
            "state={} sats={} in_view={} hdop={hdop} age_s={age} sentences={} replies={} unread={} rejected={}",
            self.sky(now_ms).word(),
            self.sats,
            self.in_view(),
            self.sentences,
            self.replies,
            self.unread,
            self.rejected
        )
    }
}

/// How often a board on its own refreshes its fix while it moves: five
/// minutes, the moving cadence of `project_announce_cadence_and_use_cases`
/// (#401, #457).
pub const MOVING_REFRESH_MS: u64 = 5 * 60 * 1000;

/// And while it is still: an hour.
pub const STILL_REFRESH_MS: u64 = 60 * 60 * 1000;

/// Two fixes this many metres apart or more say the board moved: the
/// GNSS-proven "moving" of the cadence. More than a still receiver's
/// wander at an HDOP of 2 (2 x [`UERE_M`] = 10 m), well under a walk's
/// five minutes.
pub const MOVED_M: f64 = 50.0;

/// How long a refresh holds the receiver after its first fix, so the
/// HDOP settles before the module sleeps again.
pub const SETTLE_MS: u64 = 10_000;

/// How long a refresh searches before it gives up until the next one:
/// two minutes, past a warm start's time to fix and short of a cold
/// start's under a bad sky, which the map's own wish still waits for.
pub const SEARCH_MS: u64 = 120_000;

/// When the receiver is powered (1477): while the map is open or a
/// share runs, always; otherwise for a refresh when the cadence says one
/// is due, until it has a settled fix or gave up. The module stays off
/// until something asks for it.
#[derive(Clone, Debug, Default)]
pub struct Duty {
    /// When the running refresh started, `None` when none runs.
    refresh: Option<u64>,
    /// When the last refresh ended, with or without a fix.
    last_refresh: Option<u64>,
    /// The fixes of the last two refreshes, for "moving".
    kept: [Option<Location>; 2],
    /// Asked for at least once: an image that is never asked keeps the
    /// receiver off for its whole run.
    asked: bool,
}

impl Duty {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the last two refreshes' fixes lie [`MOVED_M`] apart.
    pub fn moving(&self) -> bool {
        match &self.kept {
            [Some(a), Some(b)] => loquax::ui::share::metres(a, b) >= MOVED_M,
            _ => false,
        }
    }

    /// The cadence now: [`MOVING_REFRESH_MS`] or [`STILL_REFRESH_MS`].
    pub fn cadence_ms(&self) -> u64 {
        match self.moving() {
            true => MOVING_REFRESH_MS,
            false => STILL_REFRESH_MS,
        }
    }

    /// Whether the receiver is to be powered at `now_ms`: `wanted` is the
    /// map open or a share running, `receiver` what it says.
    pub fn on(&mut self, wanted: bool, receiver: &Receiver, now_ms: u64) -> bool {
        if wanted {
            self.asked = true;
            // A refresh that was running ends with the wish's fix.
            return true;
        }
        if !self.asked {
            return false;
        }
        if let Some(start) = self.refresh {
            let fixed = receiver
                .last()
                .is_some_and(|(_, at)| at >= start && now_ms.saturating_sub(at) < SETTLE_MS);
            let settled = receiver
                .last()
                .is_some_and(|(_, at)| at >= start && now_ms >= at + SETTLE_MS);
            let gave_up = now_ms >= start + SEARCH_MS && !fixed;
            if settled || gave_up {
                if settled {
                    let fix = receiver.last().map(|(l, _)| l);
                    self.kept = [self.kept[1], fix];
                }
                self.refresh = None;
                self.last_refresh = Some(now_ms);
                return false;
            }
            return true;
        }
        let due = match (self.last_refresh, receiver.last()) {
            (Some(last), _) => now_ms >= last + self.cadence_ms(),
            (None, Some((_, at))) => now_ms >= at + self.cadence_ms(),
            (None, None) => true,
        };
        if due {
            self.refresh = Some(now_ms);
        }
        due
    }
}

/// The map's corner words for `sky` in `lang` (1477): "Suche Satelliten…
/// 4" while it searches, the fix's age after; nothing while off.
pub fn sky_words(sky: Sky, lang: loquax::ui::strings::Lang) -> Option<String> {
    use loquax::ui::strings::{Arg, StrId, Text};
    match sky {
        Sky::Off => None,
        Sky::Searching { in_view } => Some(
            Text::Key(
                StrId::MapSkySearching,
                alloc::vec![Arg::Int(i64::from(in_view))],
            )
            .resolve(lang),
        ),
        Sky::Fix { age_s } => Some(
            Text::Key(
                StrId::MapSkyFix,
                alloc::vec![Arg::Str(age_words(age_s, lang))],
            )
            .resolve(lang),
        ),
    }
}

/// A fix's age in a few cells: `12 s`, `4 Min.`/`4 min`, `2 h`.
pub fn age_words(age_s: u32, lang: loquax::ui::strings::Lang) -> String {
    let de = matches!(lang, loquax::ui::strings::Lang::De);
    match age_s {
        s if s < 60 => alloc::format!("{s} s"),
        s if s < 3600 => alloc::format!("{} {}", s / 60, if de { "Min." } else { "min" }),
        s => alloc::format!("{} h", s / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `body` as a sentence with its checksum.
    fn sentence(body: &str) -> alloc::vec::Vec<u8> {
        let sum = body.bytes().fold(0u8, |a, b| a ^ b);
        alloc::format!("${body}*{sum:02X}").into_bytes()
    }

    #[test]
    fn a_coordinate_reads_to_microdegrees_without_a_float() {
        assert_eq!(degrees(b"4346.4000", b"N", b'S'), Some(43_773_333));
        assert_eq!(degrees(b"01115.3600", b"E", b'W'), Some(11_256_000));
        assert_eq!(degrees(b"3352.12345", b"S", b'S'), Some(-33_868_724));
        assert_eq!(degrees(b"", b"N", b'S'), None);
        assert_eq!(degrees(b"4361.0000", b"N", b'S'), None, "61 minutes");
    }

    #[test]
    fn the_date_and_time_are_unix_seconds() {
        // 2026-11-09 05:30:12 UTC.
        assert_eq!(unix(b"053012.000", b"091126"), Some(1_794_202_212));
        assert_eq!(unix(b"000000", b"011326"), None, "month 13");
        assert_eq!(unix(b"", b"091126"), None);
    }

    #[test]
    fn a_bad_checksum_is_no_sentence() {
        let mut good =
            sentence("GNGGA,053012.000,4346.4000,N,01115.3600,E,1,08,0.9,62.3,M,47.0,M,,");
        assert!(parse(&good).is_some());
        let at = good.len() - 1;
        good[at] = if good[at] == b'0' { b'1' } else { b'0' };
        assert_eq!(parse(&good), None);
    }
}
