//! What loquax reads of telemetry: the positions in an incoming message's
//! fields, as slice (a) of `docs/src/concepts/loquax-map.md` has them.
//!
//! Two fields carry a position, both in Sideband's `Telemeter` format and
//! both read with the codec of `leviculum-lxmf`:
//!
//! - `FIELD_TELEMETRY`, one node's own readings. Who measured is the
//!   message's signed source, except in a group's copy of a member's
//!   message: the home signs that with the group, and the writer travels in
//!   `lev.group_copy` (1687). That name is a claim the signature does not
//!   cover; [`read`] files under the source, and only the caller, who
//!   knows the reader's groups, may file under the claimed writer
//!   ([`read_as`], [`claimed_writer`]).
//! - `FIELD_TELEMETRY_STREAM`, a collector's rows. Who measured is the row's
//!   source hash; the message's source only delivered it (`via`).
//!
//! Every reading with a location becomes one [`Fix`]. A blob that does not
//! decode is [`Unreadable`]: kept raw with its message, and named in
//! `tui.log` by the shell, never dropped silently (the archive rule of
//! `docs/src/concepts/telemetry.md`, "What a receiving node owes").
//!
//! Nothing here sends: loquax receives and shows. The core has no clock, so
//! every time here is unix seconds handed in by the caller.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_lxmf::constants::{FIELD_TELEMETRY, FIELD_TELEMETRY_STREAM};
use leviculum_lxmf::lev::Lev;
use leviculum_lxmf::msgpack::{self, Number};
use leviculum_lxmf::telemetry::{decode_stream_field_value, Location, Telemetry};
use leviculum_lxmf::Field;

use crate::address::to_hex;
use crate::when::WallTime;

/// A fix older than this is drawn dimmed, its age still beside it: twice
/// the station profile's 60 min heartbeat, so a board that misses one report
/// stays current and one that misses two goes dim (`loquax-map.md`,
/// "Stale"; proposed there, Lew may change it). A fix is never removed for
/// its age alone.
pub const STALE_AFTER_SECS: i64 = 2 * 3600;

/// How far ahead of our clock a fix may claim to be before it is clamped:
/// honest skew between two synced clocks, not the future
/// (`docs/src/concepts/time-and-clocks.md`, "Ingress: clamp for semantics,
/// keep for display").
pub const FUTURE_TOLERANCE_SECS: i64 = 120;

/// The line a telemetry report is drawn as in the conversation.
pub const REPORT_MARK: char = '◎';

/// One position, as received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// Who measured.
    pub peer: [u8; 16],
    /// Who delivered: the message's source. Differs from `peer` for a
    /// stream row.
    pub via: [u8; 16],
    /// The message it came in, as the store's key ([`crate::ui::Message::activity`]).
    pub message: Option<u64>,
    /// When we received it, unix seconds, when the shell had said the time.
    pub received_at: Option<i64>,
    /// The reading, in the wire's integers. `last_update` is the fix time.
    pub location: Location,
    /// The packed Telemeter the reading came from, for what we cannot decode
    /// yet.
    pub telemetry: Vec<u8>,
    /// How it came: in a message, or announced to the mesh (1302).
    pub source: Source,
}

/// How a position came.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// In a message's telemetry field: kept by the store with its message.
    #[default]
    Telemetry,
    /// In a `lev.position` announce ([`crate::mesh_position`]): a state the
    /// screen holds and the store does not, since the node announces it
    /// again at its cadence.
    Mesh,
}

impl Fix {
    /// The fix time the sender claims, `Location.last_update`.
    pub fn fix_time(&self) -> i64 {
        self.location.last_update
    }

    /// Whether the claimed fix time is ahead of our clock at receive time
    /// by more than [`FUTURE_TOLERANCE_SECS`].
    pub fn ahead(&self) -> bool {
        self.received_at
            .is_some_and(|at| self.fix_time() > at.saturating_add(FUTURE_TOLERANCE_SECS))
    }

    /// The time this fix sorts at: the claim, or the receive time where the
    /// claim is ahead of our clock. Without the clamp one wrong clock would
    /// hold a peer's position at "now" for ever.
    pub fn order_time(&self) -> i64 {
        match (self.ahead(), self.received_at) {
            (true, Some(at)) => at,
            _ => self.fix_time(),
        }
    }

    /// Whether `self` replaces `other` as the peer's latest position.
    pub fn newer_than(&self, other: &Fix) -> bool {
        self.order_time() > other.order_time()
            || (self.order_time() == other.order_time() && self.fix_time() > other.fix_time())
    }

    /// Whether the fix is older than [`STALE_AFTER_SECS`] at `now`.
    pub fn stale(&self, now: i64) -> bool {
        now.saturating_sub(self.fix_time()) > STALE_AFTER_SECS
    }

    /// The battery's charge in whole percent, when the Telemeter it came
    /// in carried a battery sensor (1386).
    pub fn battery(&self) -> Option<u8> {
        let telemetry = Telemetry::decode(&self.telemetry).ok()?;
        let percent = telemetry.battery?.charge_percent.as_f64();
        percent
            .is_finite()
            .then(|| libm::round(percent.clamp(0.0, 100.0)) as u8)
    }

    /// How late it arrived: the seconds from its fix time to its receive
    /// time, when that is more than [`LATE_AFTER_SECS`] (1386); the peer
    /// was out of reach meanwhile.
    pub fn late_secs(&self) -> Option<i64> {
        let late = self.received_at? - self.fix_time();
        (late > LATE_AFTER_SECS).then_some(late)
    }
}

/// A fix that arrives more than this after its fix time arrived late
/// (1386): the segment it ends is dashed.
pub const LATE_AFTER_SECS: i64 = 30 * 60;

/// A telemetry field whose value did not decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable {
    /// The field number, or the stream field's for one of its rows.
    pub field: i64,
    /// Which part failed.
    pub detail: String,
    /// How many bytes were kept.
    pub len: usize,
}

/// What a message's fields carried.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    pub fixes: Vec<Fix>,
    pub unreadable: Vec<Unreadable>,
}

/// Whether the fields carry telemetry at all.
pub fn has_telemetry(fields: &[Field]) -> bool {
    fields
        .iter()
        .any(|(k, _)| *k == FIELD_TELEMETRY || *k == FIELD_TELEMETRY_STREAM)
}

/// The positions in `fields`, from a message whose signed source is
/// `source`, received at `received_at`: the message's own readings filed
/// under `source`, whatever writer a `lev.group_copy` names.
pub fn read(source: [u8; 16], fields: &[Field], received_at: Option<i64>) -> Read {
    read_as(source, source, fields, received_at)
}

/// The writer a group copy names (1687): `lev.group_copy`'s sender. Any
/// sender can put any address there, so the name is worth what its signed
/// source is: the caller files a fix under it only when the source is a
/// group the reader has joined whose membership lists the writer, not as
/// left (`ui::copy_writer`, the audit of 1757).
pub fn claimed_writer(fields: &[Field]) -> Option<leviculum_lxmf::lev::GroupCopy> {
    Lev::from_fields(fields).and_then(|lev| lev.group_copy)
}

/// [`read`] with the message's own readings filed under `writer`, which
/// the caller has established: the source itself, or the member of the
/// source's group a checked group copy names. `via` stays `source`.
pub fn read_as(
    writer: [u8; 16],
    source: [u8; 16],
    fields: &[Field],
    received_at: Option<i64>,
) -> Read {
    let mut out = Read::default();
    let fix = |peer: [u8; 16], location: Location, telemetry: &[u8]| Fix {
        peer,
        via: source,
        message: None,
        received_at,
        location,
        telemetry: telemetry.to_vec(),
        source: Source::Telemetry,
    };
    for (key, value) in fields {
        match *key {
            FIELD_TELEMETRY => match unwrap_bin(value) {
                Some(blob) => match Telemetry::decode(blob) {
                    Ok(telemetry) => {
                        if let Some(location) = telemetry.location {
                            out.fixes.push(fix(writer, location, blob));
                        }
                    }
                    Err(e) => out.unreadable.push(Unreadable {
                        field: *key,
                        detail: format!("the Telemeter is not a map ({e:?})"),
                        len: value.len(),
                    }),
                },
                None => out.unreadable.push(Unreadable {
                    field: *key,
                    detail: "the value is not a bin".to_string(),
                    len: value.len(),
                }),
            },
            FIELD_TELEMETRY_STREAM => match decode_stream_field_value(value) {
                Ok(rows) => {
                    for (n, row) in rows.iter().enumerate() {
                        let Ok(peer) = <[u8; 16]>::try_from(row.source.as_slice()) else {
                            out.unreadable.push(Unreadable {
                                field: *key,
                                detail: format!("row {n}: a source of {} bytes", row.source.len()),
                                len: row.telemetry.len(),
                            });
                            continue;
                        };
                        match Telemetry::decode(&row.telemetry) {
                            Ok(telemetry) => {
                                if let Some(location) = telemetry.location {
                                    out.fixes.push(fix(peer, location, &row.telemetry));
                                }
                            }
                            Err(e) => out.unreadable.push(Unreadable {
                                field: *key,
                                detail: format!(
                                    "row {n} from {}: the Telemeter is not a map ({e:?})",
                                    to_hex(&peer[..4])
                                ),
                                len: row.telemetry.len(),
                            }),
                        }
                    }
                }
                Err(e) => out.unreadable.push(Unreadable {
                    field: *key,
                    detail: format!("the stream is not an array ({e:?})"),
                    len: value.len(),
                }),
            },
            _ => {}
        }
    }
    out
}

/// The `tui.log` line for a field that did not decode.
pub fn unreadable_line(source: &[u8; 16], message_id: &[u8; 32], u: &Unreadable) -> String {
    format!(
        "TELEMETRY_UNREADABLE source={} message={} field={} bytes={} kept=raw detail={:?}",
        to_hex(source),
        to_hex(&message_id[..8]),
        u.field,
        u.len,
        u.detail
    )
}

fn unwrap_bin(value: &[u8]) -> Option<&[u8]> {
    let mut p = 0;
    msgpack::read_bin(value, &mut p).ok()
}

/// The fields as one raw msgpack map, the store's `fields` column: the
/// values are the wire's bytes, untouched.
pub fn pack_fields(fields: &[Field]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::map(&mut out, fields.len());
    for (key, value) in fields {
        msgpack::int(&mut out, *key);
        out.extend_from_slice(value);
    }
    out
}

/// The inverse of [`pack_fields`]; `None` for bytes that are not such a map.
pub fn unpack_fields(packed: &[u8]) -> Option<Vec<Field>> {
    let mut p = 0;
    let count = msgpack::map_len(packed, &mut p).ok()?;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let key = msgpack::read_int(packed, &mut p).ok()?;
        fields.push((key, msgpack::raw(packed, &mut p).ok()?.to_vec()));
    }
    Some(fields)
}

/// `43.77310 N 11.25600 E`: five decimals, a tenth of the wire's
/// micro-degrees (about a metre), rounded half away from zero.
/// What a traveller wants to know of the place `l` lies at (1684): the
/// country with its emergency numbers, the side of the road and the
/// currency (Wikidata, through [`leviculum_map::countries`]), and the
/// magnetic declination there on the fix's day (WMM2025, through
/// [`leviculum_map::declination`]), which turns a compass bearing into a
/// true one. A place in no country, or one the table has nothing for,
/// gives only what is known.
fn place_rows(rows: &mut Vec<String>, l: &Location) {
    use leviculum_map::countries::{self, Side};
    let (lat, lon) = (
        f64::from(l.latitude_e6) / 1e6,
        f64::from(l.longitude_e6) / 1e6,
    );
    if let Some(f) = countries::country_at(lat, lon).and_then(countries::facts) {
        rows.push(format!("  country   {}", f.name));
        let numbers: Vec<String> = f
            .numbers()
            .map(|n| {
                let mut services = Vec::new();
                if n.police {
                    services.push("police");
                }
                if n.ambulance {
                    services.push("ambulance");
                }
                if n.fire {
                    services.push("fire");
                }
                match services.is_empty() {
                    true => n.digits.to_string(),
                    false => format!("{} {}", n.digits, services.join("/")),
                }
            })
            .collect();
        if !numbers.is_empty() {
            rows.push(format!("  emergency {}", numbers.join(", ")));
        }
        if let Some(side) = f.driving {
            rows.push(format!(
                "  driving   on the {}",
                match side {
                    Side::Left => "left",
                    Side::Right => "right",
                }
            ));
        }
        if let Some(currency) = f.currency {
            rows.push(format!("  currency  {currency}"));
        }
    }
    let day = fix_wall(l.last_update, 0).civil();
    let year = i32::try_from(day.year)
        .ok()
        .and_then(|y| leviculum_map::declination::decimal_year(y, day.month, day.day));
    if let Some(year) = year {
        let km = f64::from(l.altitude_e2) / 100_000.0;
        let d = leviculum_map::declination::declination(lat, lon, km, year);
        if d.is_finite() {
            let tenths = libm::round(d.abs() * 10.0) as i64;
            rows.push(format!(
                "  magnetic  {}.{}° {} of true north",
                tenths / 10,
                tenths % 10,
                if d < 0.0 { "west" } else { "east" }
            ));
        }
    }
}

/// `location`'s altitude over sea level in centimetres: its ellipsoid
/// height less the geoid's undulation where it lies (1684).
pub fn above_sea_level(location: &Location) -> i32 {
    leviculum_map::geoid::height_above_sea_level_cm(
        location.latitude_e6,
        location.longitude_e6,
        location.altitude_e2,
    )
}

pub fn coordinates(location: &Location) -> String {
    format!(
        "{} {}",
        degrees(location.latitude_e6, 'N', 'S'),
        degrees(location.longitude_e6, 'E', 'W')
    )
}

fn degrees(e6: i32, positive: char, negative: char) -> String {
    let abs = i64::from(e6).abs();
    let e5 = (abs + 5) / 10;
    let hemisphere = if e6 < 0 { negative } else { positive };
    format!("{}.{:05} {hemisphere}", e5 / 100_000, e5 % 100_000)
}

/// `±5 m`, or `None` where the sender gave none. `Location.pack` always
/// writes the field, so zero is the only absence the wire can say: no
/// receiver reports a fix as exact to the centimetre and below.
pub fn accuracy(location: &Location) -> Option<String> {
    match location.accuracy_e2 {
        0 => None,
        e2 if e2 < 100 => Some(format!("±0.{e2:02} m")),
        e2 => Some(format!("±{} m", (u32::from(e2) + 50) / 100)),
    }
}

/// How long ago `secs` seconds is, as the header says it: `<1 min`,
/// `4 min`, `3 h`, `2 d`.
pub fn span(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        s if s < 60 => "<1 min".to_string(),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
    }
}

/// The fix time as a wall time in `offset_secs`.
pub fn fix_wall(fix_time: i64, offset_secs: i32) -> WallTime {
    WallTime {
        unix_secs: fix_time,
        offset_secs,
    }
}

/// The header's position text, longest first, at `now`: the renderer takes
/// the first that fits. A fix ahead of our clock says its claimed time and
/// that it is ahead, never an age below zero.
pub fn header_words(fix: &Fix, now: Option<&WallTime>) -> Vec<String> {
    let at = format!("at {}", coordinates(&fix.location));
    let accuracy = accuracy(&fix.location);
    let offset = now.map_or(0, |n| n.offset_secs);
    let age = match (fix.ahead(), now) {
        (true, _) => None,
        (false, Some(now)) => Some(span(now.unix_secs - fix.fix_time())),
        (false, None) => None,
    };
    let when = match &age {
        Some(age) => format!("{age} ago"),
        None => format!(
            "fix {}, ahead of our clock",
            fix_wall(fix.fix_time(), offset).hh_mm()
        ),
    };
    let short = match &age {
        Some(age) => format!("pos {age}"),
        None => "pos ahead".to_string(),
    };
    let mut long = vec![at.clone()];
    long.extend(accuracy);
    long.push(when.clone());
    vec![long.join(" · "), format!("{at} · {when}"), short]
}

/// The fix a report's line names, and the one `Enter` on it opens the map
/// at: the sender's own where the message carries one beside a collector's
/// rows, else its last row.
pub fn shown_fix(read: &Read) -> Option<&Fix> {
    let own = read.fixes.iter().rev().find(|f| f.peer == f.via);
    own.or(read.fixes.last())
}

/// The conversation's line for a telemetry report: `◎ position 43.77310 N
/// 11.25600 E`. The block's own time beside it says when it arrived; the
/// fix's age is the header's. A report without a position, or one that did
/// not decode, says so on the same line.
///
/// The conversation is with the sender, so the line names the sender's own
/// fix where the message carries one beside a collector's rows; a message
/// of rows alone names its last row, as before.
pub fn report_line(read: &Read) -> String {
    match shown_fix(read) {
        Some(fix) => {
            let more = match read.fixes.len() {
                1 => String::new(),
                n => format!(" (+{} more)", n - 1),
            };
            format!(
                "{REPORT_MARK} position {}{more}",
                coordinates(&fix.location)
            )
        }
        None if !read.unreadable.is_empty() => {
            format!("{REPORT_MARK} telemetry, unreadable (kept)")
        }
        None => format!("{REPORT_MARK} telemetry, no position"),
    }
}

/// What a row folded from a run of `count` reports adds to its position
/// line, the first of them at `since` (1184): ` · 14 updates since 14:45`;
/// nothing for a report alone.
pub fn run_words(count: usize, since: Option<&WallTime>) -> String {
    match (count, since) {
        (0 | 1, _) => String::new(),
        (n, Some(at)) => format!(" · {n} updates since {}", at.hh_mm()),
        (n, None) => format!(" · {n} updates"),
    }
}

/// The detail view's rows for the telemetry in `fields`: every sensor the
/// codec decoded, per reading, with the peer of a stream row named. A fix
/// time ahead of `received_at` by more than [`FUTURE_TOLERANCE_SECS`] is
/// shown as claimed, marked.
pub fn detail_rows(
    source: &[u8; 16],
    fields: &[Field],
    offset_secs: i32,
    received_at: Option<i64>,
) -> Vec<String> {
    let when = When {
        offset_secs,
        received_at,
    };
    let mut rows = Vec::new();
    for (key, value) in fields {
        match *key {
            FIELD_TELEMETRY => {
                rows.push("telemetry".to_string());
                match unwrap_bin(value).map(Telemetry::decode) {
                    Some(Ok(telemetry)) => sensors(&mut rows, &telemetry, &when),
                    _ => rows.push(format!("  unreadable, {} bytes kept", value.len())),
                }
            }
            FIELD_TELEMETRY_STREAM => match decode_stream_field_value(value) {
                Ok(entries) => {
                    for entry in entries {
                        let from = match <[u8; 16]>::try_from(entry.source.as_slice()) {
                            Ok(peer) if &peer == source => "the sender".to_string(),
                            Ok(peer) => to_hex(&peer),
                            Err(_) => format!("a source of {} bytes", entry.source.len()),
                        };
                        rows.push(format!("telemetry of {from}, relayed"));
                        match Telemetry::decode(&entry.telemetry) {
                            Ok(telemetry) => sensors(&mut rows, &telemetry, &when),
                            Err(_) => rows.push(format!(
                                "  unreadable, {} bytes kept",
                                entry.telemetry.len()
                            )),
                        }
                    }
                }
                Err(_) => {
                    rows.push("telemetry stream".to_string());
                    rows.push(format!("  unreadable, {} bytes kept", value.len()));
                }
            },
            _ => {}
        }
    }
    rows
}

/// The offset fix times are shown in, and when the message arrived.
struct When {
    offset_secs: i32,
    received_at: Option<i64>,
}

fn sensors(rows: &mut Vec<String>, t: &Telemetry, when: &When) {
    let offset_secs = when.offset_secs;
    let before = rows.len();
    if let Some(l) = &t.location {
        rows.push(format!("  position  {}", coordinates(l)));
        place_rows(rows, l);
        // The wire's altitude is the GNSS height over the WGS84 ellipsoid,
        // as Android and Sideband report it; shown over sea level by the
        // EGM2008 geoid (1684), which is up to a hundred metres apart.
        rows.push(format!(
            "  altitude  {} m above sea level",
            hundredths(i64::from(above_sea_level(l)))
        ));
        rows.push(format!(
            "  speed     {} m/s",
            hundredths(i64::from(l.speed_e2))
        ));
        rows.push(format!(
            "  bearing   {}°",
            hundredths(i64::from(l.bearing_e2))
        ));
        if let Some(accuracy) = accuracy(l) {
            rows.push(format!("  accuracy  {accuracy}"));
        }
        let wall = fix_wall(l.last_update, offset_secs);
        let c = wall.civil();
        // Out of reach meanwhile (1386): the map dashes the line it ends.
        let late = when.received_at.map(|at| at - l.last_update);
        let late = late.filter(|&secs| secs > LATE_AFTER_SECS);
        let ahead = when
            .received_at
            .is_some_and(|at| l.last_update > at.saturating_add(FUTURE_TOLERANCE_SECS));
        rows.push(format!(
            "  fix       {:04}-{:02}-{:02} {}{}",
            c.year,
            c.month,
            c.day,
            wall.hh_mm_ss(),
            if ahead {
                "  ! ahead of our clock, as claimed"
            } else {
                ""
            }
        ));
        if let Some(secs) = late {
            rows.push(format!("  arrived   {} late", span(secs)));
        }
    }
    if let Some(time) = t.time {
        rows.push(format!(
            "  time      {}",
            fix_wall(time, offset_secs).hh_mm_ss()
        ));
    }
    if let Some(b) = &t.battery {
        let charging = match b.charging {
            Some(true) => ", charging",
            Some(false) => ", not charging",
            None => "",
        };
        let temperature = b
            .temperature
            .map(|t| format!(", {} °C", number(t)))
            .unwrap_or_default();
        rows.push(format!(
            "  battery   {} %{charging}{temperature}",
            number(b.charge_percent)
        ));
    }
    if let Some(link) = &t.physical_link {
        let mut words = Vec::new();
        if let Some(rssi) = link.rssi {
            words.push(format!("rssi {} dBm", number(rssi)));
        }
        if let Some(snr) = link.snr {
            words.push(format!("snr {} dB", number(snr)));
        }
        if let Some(q) = link.q {
            words.push(format!("quality {} %", number(q)));
        }
        if !words.is_empty() {
            rows.push(format!("  link      {}", words.join(", ")));
        }
    }
    if let Some(temperature) = t.temperature {
        rows.push(format!("  temp      {} °C", number(temperature)));
    }
    if let Some(producers) = &t.power_production {
        for p in producers {
            let label = p.type_label.as_deref().unwrap_or("power");
            rows.push(format!("  {label}  {} W", number(p.power)));
        }
    }
    if rows.len() == before {
        rows.push("  no sensor with a reading".to_string());
    }
}

/// A wire integer in hundredths, as a decimal: `-12.05`.
fn hundredths(e2: i64) -> String {
    let sign = if e2 < 0 { "-" } else { "" };
    let abs = e2.abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// A number as the sender sent it: an integer bare, a float to two places.
fn number(n: Number) -> String {
    match n {
        Number::Int(v) => format!("{v}"),
        Number::Float(v) => {
            let scaled = v * 100.0;
            // Round half away from zero without libm.
            let rounded = if scaled < 0.0 {
                (scaled - 0.5) as i64
            } else {
                (scaled + 0.5) as i64
            };
            hundredths(rounded)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::telemetry::{encode_stream_field_value, Battery, StreamEntry};

    const BOB: [u8; 16] = [0xb0; 16];
    const CAROL: [u8; 16] = [0xca; 16];
    const COLLECTOR: [u8; 16] = [0xc0; 16];
    const NOW: i64 = 1_793_016_000;

    fn location(lat: i32, lon: i32, accuracy: u16, at: i64) -> Location {
        Location {
            latitude_e6: lat,
            longitude_e6: lon,
            altitude_e2: 5_120,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: accuracy,
            last_update: at,
        }
    }

    fn report(location: Location) -> Telemetry {
        Telemetry {
            time: Some(location.last_update),
            location: Some(location),
            battery: Some(Battery {
                charge_percent: Number::Float(87.0),
                charging: Some(false),
                temperature: None,
            }),
            ..Telemetry::default()
        }
    }

    #[test]
    fn a_sideband_report_is_one_fix_of_its_sender() {
        let fields = vec![(
            FIELD_TELEMETRY,
            report(location(43_773_100, 11_256_000, 500, NOW - 240)).encode_field_value(),
        )];
        let read = read(BOB, &fields, Some(NOW));
        assert!(read.unreadable.is_empty());
        assert_eq!(read.fixes.len(), 1);
        let fix = &read.fixes[0];
        assert_eq!((fix.peer, fix.via), (BOB, BOB));
        assert_eq!(fix.fix_time(), NOW - 240);
        assert_eq!(coordinates(&fix.location), "43.77310 N 11.25600 E");
        assert_eq!(accuracy(&fix.location).as_deref(), Some("±5 m"));
    }

    /// Bytes as Sideband's `Telemeter.packed()` writes them for a time
    /// sensor and a location sensor beside a battery sensor mapped to nil
    /// (`Sensor.pack` returning `None`), built by hand from the format and
    /// not by our encoder: SID 0x01 an int, SID 0x02 the seven-element
    /// array of `Location.pack`, SID 0x04 nil.
    #[test]
    fn a_hand_packed_sideband_blob_with_a_nil_sensor_reads() {
        let mut blob = vec![0x83, 0x01, 0xce];
        blob.extend_from_slice(&((NOW - 60) as u32).to_be_bytes());
        blob.extend_from_slice(&[0x02, 0x97]);
        for (bytes, width) in [
            (52_520_008i32.to_be_bytes().to_vec(), 4),
            ((-13_404_954i32).to_be_bytes().to_vec(), 4),
            (3_400i32.to_be_bytes().to_vec(), 4),
            (0u32.to_be_bytes().to_vec(), 4),
            (0i32.to_be_bytes().to_vec(), 4),
            (1_200u16.to_be_bytes().to_vec(), 2),
        ] {
            blob.extend_from_slice(&[0xc4, width]);
            blob.extend_from_slice(&bytes);
        }
        blob.push(0xce);
        blob.extend_from_slice(&((NOW - 60) as u32).to_be_bytes());
        blob.extend_from_slice(&[0x04, 0xc0]);
        let mut value = vec![0xc4, blob.len() as u8];
        value.extend_from_slice(&blob);
        let read = read(BOB, &[(FIELD_TELEMETRY, value)], Some(NOW));
        assert_eq!(read.fixes.len(), 1, "{read:?}");
        assert_eq!(
            coordinates(&read.fixes[0].location),
            "52.52001 N 13.40495 W"
        );
        assert_eq!(accuracy(&read.fixes[0].location).as_deref(), Some("±12 m"));
    }

    #[test]
    fn a_stream_row_belongs_to_who_measured_and_names_who_delivered() {
        let rows = [
            (BOB, location(43_773_100, 11_256_000, 500, NOW - 600)),
            (CAROL, location(-33_868_800, 151_209_300, 0, NOW - 300)),
        ]
        .map(|(peer, l)| StreamEntry {
            source: peer.to_vec(),
            timestamp: Number::Int(l.last_update),
            telemetry: report(l).encode(),
            appearance: None,
        });
        let fields = vec![(FIELD_TELEMETRY_STREAM, encode_stream_field_value(&rows))];
        let read = read(COLLECTOR, &fields, Some(NOW));
        let who: Vec<_> = read.fixes.iter().map(|f| (f.peer, f.via)).collect();
        assert_eq!(who, vec![(BOB, COLLECTOR), (CAROL, COLLECTOR)]);
        assert_eq!(
            coordinates(&read.fixes[1].location),
            "33.86880 S 151.20930 E"
        );
        assert_eq!(accuracy(&read.fixes[1].location), None, "none given");
    }

    /// 1687: the home sends a group's copy of a member's message under the
    /// group's own address, so the message's signed source is the group and
    /// the writer travels in `lev.group_copy`. The claim is read out for
    /// the caller, who knows the reader's groups; [`read`] alone files
    /// under the signed source (the audit of 1757: a stranger's "copy"
    /// moved anyone's pin), [`read_as`] under the writer the caller
    /// established, the group as `via`.
    #[test]
    fn a_group_copys_writer_is_a_claim_the_caller_checks() {
        use leviculum_lxmf::lev::{GroupCopy, Lev};

        const GROUP: [u8; 16] = [0x60; 16];
        let mut fields: Vec<Field> = Lev {
            group_copy: Some(GroupCopy {
                sender: BOB,
                sent_ms: 1_793_015_000_000,
                epoch: 3,
            }),
            ..Lev::default()
        }
        .fields()
        .to_vec();
        fields.push((
            FIELD_TELEMETRY,
            report(location(43_773_100, 11_256_000, 500, NOW - 240)).encode_field_value(),
        ));
        let claim = claimed_writer(&fields).expect("a claim");
        assert_eq!((claim.sender, claim.epoch), (BOB, 3));
        let read = read(GROUP, &fields, Some(NOW));
        assert!(read.unreadable.is_empty());
        let who: Vec<_> = read.fixes.iter().map(|f| (f.peer, f.via)).collect();
        assert_eq!(who, [(GROUP, GROUP)], "the signed source, not the claim");
        let read = read_as(BOB, GROUP, &fields, Some(NOW));
        let who: Vec<_> = read.fixes.iter().map(|f| (f.peer, f.via)).collect();
        assert_eq!(
            who,
            [(BOB, GROUP)],
            "the writer measured it; the group delivered the copy"
        );
        assert_eq!(claimed_writer(&[]), None);
    }

    fn stream(rows: &[([u8; 16], Location)]) -> Vec<u8> {
        let rows: Vec<StreamEntry> = rows
            .iter()
            .map(|(peer, l)| StreamEntry {
                source: peer.to_vec(),
                timestamp: Number::Int(l.last_update),
                telemetry: report(*l).encode(),
                appearance: None,
            })
            .collect();
        encode_stream_field_value(&rows)
    }

    /// A message with the sender's own reading and a collector's rows: the
    /// line names the sender's fix, whichever field came first.
    #[test]
    fn the_report_line_names_the_senders_own_fix_beside_a_stream() {
        let rows = stream(&[
            (CAROL, location(43_776_000, 11_266_000, 0, NOW - 180)),
            (COLLECTOR, location(43_774_000, 11_258_000, 0, NOW - 1)),
        ]);
        let own = (
            FIELD_TELEMETRY,
            report(location(43_775_000, 11_261_000, 0, NOW - 4)).encode_field_value(),
        );
        let line = "◎ position 43.77500 N 11.26100 E (+2 more)";
        let fields = vec![own.clone(), (FIELD_TELEMETRY_STREAM, rows.clone())];
        assert_eq!(report_line(&read(BOB, &fields, Some(NOW))), line);
        let fields = vec![(FIELD_TELEMETRY_STREAM, rows), own];
        assert_eq!(report_line(&read(BOB, &fields, Some(NOW))), line);
    }

    /// Rows alone: none is the sender's, the line names the last row as it
    /// did before the sender's own fix was preferred.
    #[test]
    fn the_report_line_of_a_stream_alone_names_its_last_row() {
        let rows = stream(&[
            (CAROL, location(43_776_000, 11_266_000, 0, NOW - 180)),
            (BOB, location(43_774_000, 11_258_000, 0, NOW - 1)),
        ]);
        let fields = vec![(FIELD_TELEMETRY_STREAM, rows)];
        assert_eq!(
            report_line(&read(COLLECTOR, &fields, Some(NOW))),
            "◎ position 43.77400 N 11.25800 E (+1 more)"
        );
    }

    #[test]
    fn a_broken_blob_is_unreadable_and_named_not_dropped() {
        let fields = vec![
            (FIELD_TELEMETRY, vec![0xc4, 0x02, 0x91, 0x01]),
            (FIELD_TELEMETRY_STREAM, vec![0x01]),
        ];
        let read = read(BOB, &fields, Some(NOW));
        assert!(read.fixes.is_empty());
        assert_eq!(read.unreadable.len(), 2);
        let line = unreadable_line(&BOB, &[0x42; 32], &read.unreadable[0]);
        assert!(
            line.starts_with("TELEMETRY_UNREADABLE source=b0b0"),
            "{line}"
        );
        assert!(line.contains("field=2 bytes=4 kept=raw"), "{line}");
        assert_eq!(report_line(&read), "◎ telemetry, unreadable (kept)");
    }

    #[test]
    fn a_fix_ahead_of_our_clock_sorts_at_receive_time_and_shows_its_claim() {
        let ahead = Fix {
            peer: BOB,
            via: BOB,
            message: None,
            received_at: Some(NOW),
            location: location(1, 1, 0, NOW + 3600),
            telemetry: Vec::new(),
            source: Source::Telemetry,
        };
        assert!(ahead.ahead());
        assert_eq!(ahead.order_time(), NOW);
        let honest = Fix {
            location: location(1, 1, 0, NOW - 60),
            ..ahead.clone()
        };
        assert!(!honest.ahead());
        assert!(ahead.newer_than(&honest), "now beats a minute ago");
        let later = Fix {
            location: location(1, 1, 0, NOW + 60),
            received_at: Some(NOW + 30),
            ..ahead.clone()
        };
        assert!(!later.ahead(), "inside the tolerance");
        assert!(later.newer_than(&ahead), "the clamped one sorts at NOW");
        let now = WallTime {
            unix_secs: NOW,
            offset_secs: 0,
        };
        let words = header_words(&ahead, Some(&now));
        assert!(
            words[0].ends_with("fix 13:00, ahead of our clock"),
            "{words:?}"
        );
        assert_eq!(words.last().map(String::as_str), Some("pos ahead"));
        let fields = vec![(
            FIELD_TELEMETRY,
            Telemetry {
                location: Some(ahead.location),
                ..Telemetry::default()
            }
            .encode_field_value(),
        )];
        let rows = detail_rows(&BOB, &fields, 0, Some(NOW));
        assert!(
            rows.contains(
                &"  fix       2026-10-26 13:00:00  ! ahead of our clock, as claimed".to_string()
            ),
            "{rows:?}"
        );
    }

    #[test]
    fn the_stale_boundary_is_two_hours() {
        let fix = Fix {
            peer: BOB,
            via: BOB,
            message: None,
            received_at: Some(NOW),
            location: location(1, 1, 0, NOW - STALE_AFTER_SECS),
            telemetry: Vec::new(),
            source: Source::Telemetry,
        };
        assert!(!fix.stale(NOW), "exactly two hours is still current");
        assert!(fix.stale(NOW + 1));
    }

    #[test]
    fn the_header_says_position_accuracy_and_age_longest_first() {
        let fix = read(
            BOB,
            &[(
                FIELD_TELEMETRY,
                report(location(43_773_100, 11_256_000, 500, NOW - 240)).encode_field_value(),
            )],
            Some(NOW),
        )
        .fixes
        .remove(0);
        let now = WallTime {
            unix_secs: NOW,
            offset_secs: 3600,
        };
        assert_eq!(
            header_words(&fix, Some(&now)),
            vec![
                "at 43.77310 N 11.25600 E · ±5 m · 4 min ago".to_string(),
                "at 43.77310 N 11.25600 E · 4 min ago".to_string(),
                "pos 4 min".to_string(),
            ]
        );
    }

    #[test]
    fn the_detail_rows_list_every_decoded_sensor() {
        let fields = vec![(
            FIELD_TELEMETRY,
            report(location(43_773_100, 11_256_000, 500, NOW - 240)).encode_field_value(),
        )];
        let rows = detail_rows(&BOB, &fields, 0, Some(NOW));
        assert_eq!(rows[0], "telemetry");
        assert!(rows.contains(&"  position  43.77310 N 11.25600 E".to_string()));
        // 51.20 m over the ellipsoid in Florence, where the geoid lies
        // 45.60 m above it (EGM2008, 1684): 5.60 m above sea level.
        assert!(
            rows.contains(&"  altitude  5.60 m above sea level".to_string()),
            "{rows:?}"
        );
        // The place's facts (1684): Italy, by Wikidata, and the WMM2025
        // declination in Florence on 2026-10-26, rounded to a tenth (the
        // model itself is held to NOAA's published test values in
        // leviculum-map's `declination` test).
        for row in [
            "  country   Italy",
            "  driving   on the right",
            "  currency  EUR",
        ] {
            assert!(rows.contains(&row.to_string()), "{row:?} in {rows:?}");
        }
        assert!(
            rows.iter().any(|r| r.starts_with("  emergency 112")),
            "{rows:?}"
        );
        assert!(
            rows.contains(&"  magnetic  4.0° east of true north".to_string()),
            "{rows:?}"
        );
        assert!(rows.contains(&"  accuracy  ±5 m".to_string()));
        assert!(rows.contains(&"  battery   87.00 %, not charging".to_string()));
    }

    /// 1386: the battery a fix's Telemeter carried, in whole percent; a
    /// fix that arrived 2 h after its fix time is late and its detail says
    /// so, one 20 min after is not.
    #[test]
    fn a_fix_knows_its_battery_and_whether_it_arrived_late() {
        let late = report(location(43_773_100, 11_256_000, 500, NOW - 7_200));
        let fields = vec![(FIELD_TELEMETRY, late.encode_field_value())];
        let fix = read(BOB, &fields, Some(NOW)).fixes.remove(0);
        assert_eq!(fix.battery(), Some(87));
        assert_eq!(fix.late_secs(), Some(7_200));
        let rows = detail_rows(&BOB, &fields, 0, Some(NOW));
        assert!(
            rows.contains(&"  arrived   2 h late".to_string()),
            "{rows:?}"
        );

        let soon = report(location(43_773_100, 11_256_000, 500, NOW - 1_200));
        let fields = vec![(FIELD_TELEMETRY, soon.encode_field_value())];
        let fix = read(BOB, &fields, Some(NOW)).fixes.remove(0);
        assert_eq!(fix.late_secs(), None);
        let rows = detail_rows(&BOB, &fields, 0, Some(NOW));
        assert!(!rows.iter().any(|r| r.contains("arrived")), "{rows:?}");
        let bare = Fix {
            telemetry: Vec::new(),
            ..fix
        };
        assert_eq!(bare.battery(), None);
    }

    #[test]
    fn fields_pack_and_unpack_untouched() {
        let fields = vec![
            (FIELD_TELEMETRY, vec![0xc4, 0x01, 0x80]),
            (0x10, vec![0xa1, b'x']),
        ];
        let packed = pack_fields(&fields);
        assert_eq!(unpack_fields(&packed), Some(fields));
        assert_eq!(unpack_fields(&[0x01]), None);
    }

    #[test]
    fn decimals_round_at_the_fifth_place() {
        let l = location(-15, 999_995, 99, 0);
        assert_eq!(coordinates(&l), "0.00002 S 1.00000 E");
        assert_eq!(accuracy(&l).as_deref(), Some("±0.99 m"));
        assert_eq!(span(59), "<1 min");
        assert_eq!(span(3599), "59 min");
        assert_eq!(span(7200), "2 h");
        assert_eq!(span(-5), "<1 min");
    }
}
