//! What the helper says, and keeps, when a telemetry message arrives.
//!
//! A Columba or Sideband position report is an LXMF message with an **empty
//! body**: the reading rides in `FIELD_TELEMETRY`, and `content` and `title`
//! are empty because Sideband suppresses the notification only when both are
//! (`leviculum-lxmf/src/telemetry.rs`, `build_report`). So the helper's
//! `lxmf_msg_received` line — source, base64 body, signature validity — says
//! *nothing* about such a message beyond who sent it. This module is the
//! second line it gets, and the file it lands in.
//!
//! # Two sinks, one rendering
//!
//! Every reading is rendered exactly once, into [`Column`]s, and both sinks
//! print those columns:
//!
//! * one `EVENT lxmf_telemetry_received …` line, so a walk is readable in
//!   the same stream as everything else the helper says;
//! * one JSON line appended to `<LXMF_STORAGE>/telemetry.jsonl`, so the
//!   positions survive a helper restart and a log rotation — the stdout
//!   stream belongs to whoever spawned the process, the file does not.
//!
//! A single renderer is what keeps the two from drifting: a column absent
//! on the event line prints `none` and in the row prints `null`, and there
//! is no third place that decides which sensors exist.
//!
//! # The rule this module inherits
//!
//! **The raw blob goes out on every line.** `Telemeter.from_packed` skips a
//! sensor ID it has no class for and our decoder skips it too — right for a
//! codec, wrong for an archive. `fields_hex` (and the row's `raw`) is
//! therefore the packed blob exactly as it arrived, so a field test that
//! meets a sensor nobody has implemented yet still has the bytes. A blob
//! that does not decode at all becomes `lxmf_telemetry_undecodable` rather
//! than a message on the floor. This is `lntd`'s rule
//! (`leviculum-cli/src/lntd_store.rs`, and `docs/src/concepts/telemetry.md`,
//! "What a receiving node owes"), applied to a helper that has no database.
//!
//! One deliberate divergence from `lntd`: an empty Telemeter map still
//! produces a line here. `lntd` drops it because a row that says nothing is
//! not worth archiving; the helper reports it because "the phone sent a
//! reading with nothing in it" is exactly the kind of thing a field test
//! needs to see rather than infer from silence.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use leviculum_lxmf::constants::{FIELD_TELEMETRY, FIELD_TELEMETRY_STREAM};
use leviculum_lxmf::msgpack::{self, Number};
use leviculum_lxmf::telemetry::{decode_stream_field_value, PowerProducer, Telemetry};
use leviculum_lxmf::Message;

use crate::protocol::hex_encode;

/// The event a decoded reading emits.
pub const EVENT_RECEIVED: &str = "lxmf_telemetry_received";
/// The event a blob we could not read emits. Same `src` and `fields_hex`,
/// so the bytes are recoverable from the log alone.
pub const EVENT_UNDECODABLE: &str = "lxmf_telemetry_undecodable";
/// The durable half, under `LXMF_STORAGE`.
pub const LOG_FILE: &str = "telemetry.jsonl";

/// What one telemetry-carrying field value contributed.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// The blob is a Telemeter map. Sensors we have no arm for are in
    /// [`Report::raw`] and nowhere else.
    Sensors(Box<Telemetry>),
    /// It is not one, with the reason. The bytes are kept regardless.
    Undecodable(String),
}

/// One reading on its way to both sinks.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Who measured. The message's source for a `FIELD_TELEMETRY` reading;
    /// for a `FIELD_TELEMETRY_STREAM` row it is the row's own source, which
    /// is the whole point of a collector exchange — the rows are other
    /// people's readings.
    pub source: Vec<u8>,
    /// Who delivered it. Equal to `source` unless the message was a
    /// collector's stream, which is why the key is worth its width: without
    /// it a relayed row and a first-hand one look the same.
    pub via: [u8; 16],
    /// The packed Telemeter blob, exactly as it arrived.
    pub raw: Vec<u8>,
    pub reading: Reading,
}

/// Every reading one arriving message carries, in field order.
///
/// Empty for the ordinary case: somebody's chat message to an address that
/// also collects. A message carrying both field numbers yields both, which
/// is what "and/or" on the wire means.
pub fn reports(message: &Message) -> Vec<Report> {
    let mut out = Vec::new();
    for (id, value) in &message.fields {
        match *id {
            FIELD_TELEMETRY => out.push(direct(message, value)),
            FIELD_TELEMETRY_STREAM => out.extend(stream(message, value)),
            _ => {}
        }
    }
    out
}

/// A first-hand reading: `FIELD_TELEMETRY`.
fn direct(message: &Message, value: &[u8]) -> Report {
    // Sideband stores the packed bytes in the field, so the value is a
    // msgpack bin wrapping the blob (`Telemetry::encode_field_value`). A
    // value that is not even that is kept as itself — there is no inner blob
    // to unwrap — which is `lntd`'s disposition too
    // (`leviculum-cli/src/lntd_store.rs`, `ingest`).
    let mut p = 0;
    match msgpack::read_bin(value, &mut p) {
        Ok(blob) => Report {
            source: message.source_hash.to_vec(),
            via: message.source_hash,
            raw: blob.to_vec(),
            reading: decode(blob),
        },
        Err(e) => Report {
            source: message.source_hash.to_vec(),
            via: message.source_hash,
            raw: value.to_vec(),
            reading: Reading::Undecodable(format!("FIELD_TELEMETRY is not a msgpack bin: {e:?}")),
        },
    }
}

/// A collector's answer: `FIELD_TELEMETRY_STREAM`, one report per row.
///
/// The decoder already drops a row it cannot read and keeps the rest
/// (`decode_stream_field_value`), so a malformed row costs its own line and
/// not the message. Only a value that is not an array at all leaves nothing
/// to iterate, and that becomes one undecodable report carrying the whole
/// field value.
fn stream(message: &Message, value: &[u8]) -> Vec<Report> {
    match decode_stream_field_value(value) {
        Ok(entries) => entries
            .into_iter()
            .map(|entry| Report {
                source: entry.source,
                via: message.source_hash,
                reading: decode(&entry.telemetry),
                raw: entry.telemetry,
            })
            .collect(),
        Err(e) => vec![Report {
            source: message.source_hash.to_vec(),
            via: message.source_hash,
            raw: value.to_vec(),
            reading: Reading::Undecodable(format!(
                "FIELD_TELEMETRY_STREAM is not an array of rows: {e:?}"
            )),
        }],
    }
}

fn decode(blob: &[u8]) -> Reading {
    match Telemetry::decode(blob) {
        Ok(telemetry) => Reading::Sensors(Box::new(telemetry)),
        Err(e) => Reading::Undecodable(format!("not a Telemeter map: {e:?}")),
    }
}

/// One sensor value, rendered once for both sinks.
///
/// `value` is always a bare JSON number or boolean literal, which is also
/// exactly what the `EVENT` grammar wants: no quotes and no whitespace, so
/// the driver's `split_whitespace` tokeniser sees one field. Absence prints
/// `none` on the event line and `null` in the row — never a missing key, so
/// the key set is the same on every message and `awk` works on it.
struct Column {
    key: &'static str,
    value: Option<String>,
}

impl Column {
    fn new(key: &'static str, value: Option<String>) -> Self {
        Self { key, value }
    }
}

/// The sensors this build decodes, in one fixed order.
///
/// Exactly the set `leviculum-lxmf/src/telemetry.rs` has an arm for today:
/// `SID_TIME`, `SID_LOCATION` (seven values, `fix_time` being the reading's
/// own `last_update` rather than the message's clock), `SID_BATTERY`,
/// `SID_PHYSICAL_LINK`, `SID_TEMPERATURE` and `SID_POWER_PRODUCTION`. A
/// sensor added to that file gets a column here and the key set grows by
/// one; nothing already in it ever moves or changes meaning.
fn columns(telemetry: &Telemetry) -> Vec<Column> {
    let location = telemetry.location;
    let battery = telemetry.battery;
    let link = telemetry.physical_link;
    vec![
        Column::new("time", telemetry.time.map(|t| t.to_string())),
        Column::new("lat", location.map(|l| scaled(i64::from(l.latitude_e6), 6))),
        Column::new(
            "lon",
            location.map(|l| scaled(i64::from(l.longitude_e6), 6)),
        ),
        Column::new("alt", location.map(|l| scaled(i64::from(l.altitude_e2), 2))),
        Column::new("speed", location.map(|l| scaled(i64::from(l.speed_e2), 2))),
        Column::new(
            "bearing",
            location.map(|l| scaled(i64::from(l.bearing_e2), 2)),
        ),
        Column::new(
            "accuracy",
            location.map(|l| scaled(i64::from(l.accuracy_e2), 2)),
        ),
        Column::new("fix_time", location.map(|l| l.last_update.to_string())),
        Column::new(
            "battery_pct",
            battery.and_then(|b| number(b.charge_percent)),
        ),
        Column::new(
            "battery_charging",
            battery.and_then(|b| b.charging).map(|c| c.to_string()),
        ),
        Column::new(
            "battery_temp_c",
            battery.and_then(|b| b.temperature).and_then(number),
        ),
        Column::new("rssi", link.and_then(|l| l.rssi).and_then(number)),
        Column::new("snr", link.and_then(|l| l.snr).and_then(number)),
        Column::new("link_q", link.and_then(|l| l.q).and_then(number)),
        Column::new("temp_c", telemetry.temperature.and_then(number)),
        Column::new(
            "power_w",
            telemetry.power_production.as_deref().and_then(total_watts),
        ),
    ]
}

/// Watts across every producer in the reading — `lntd`'s curve column
/// (`leviculum-cli/src/lntd_store.rs`, `total_watts`). The split across
/// several stays in `producers` and, whole, in the raw blob.
fn total_watts(producers: &[PowerProducer]) -> Option<String> {
    let watts: f64 = producers.iter().map(|p| p.power.as_f64()).sum();
    // No JSON literal for a non-finite sum, and a producer that reported one
    // has told us nothing anyway; it reads as absent, like every other
    // unusable number here.
    number(Number::Float(watts))
}

/// The same reading spelled for an operator: `label=watts`, `;`-joined,
/// with the reference's unnamed default slot written as `default`
/// (`leviculum-cli/src/lntd_store.rs`, `describe_producers`). Row-only: it
/// is the one value that is a string rather than a number, and the event
/// grammar has no room for a `;`-joined list.
fn describe_producers(producers: &[PowerProducer]) -> String {
    producers
        .iter()
        .map(|p| {
            let label = p.type_label.as_deref().unwrap_or("default");
            let watts = p.power.as_f64();
            format!("{label}={watts:?}")
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Render a scaled integer as an exact decimal with `decimals` places.
///
/// Exact on purpose. The wire carries scaled integers
/// (`leviculum-lxmf/src/telemetry.rs`, `Location`), and routing them through
/// an `f64` to print them would put rounding noise into the one line a
/// field walk is read from. The sign is carried separately from the
/// magnitude because `-1` at 1e6 is `-0.000001`, where integer division
/// alone would print `0.-000001`.
fn scaled(value: i64, decimals: u32) -> String {
    let scale = 10u64.pow(decimals);
    let sign = if value < 0 { "-" } else { "" };
    let magnitude = value.unsigned_abs();
    format!(
        "{sign}{}.{:0width$}",
        magnitude / scale,
        magnitude % scale,
        width = decimals as usize
    )
}

/// A wire number as a JSON literal, keeping its family: an integer prints as
/// one, a float in Rust's shortest round-trip form.
///
/// A non-finite float has no JSON literal, so it reads as absent rather than
/// as `NaN`, which no parser on the other side would take — and a sensor
/// that reported one has said nothing.
fn number(value: Number) -> Option<String> {
    match value {
        Number::Int(v) => Some(v.to_string()),
        Number::Float(v) if v.is_finite() => Some(format!("{v:?}")),
        Number::Float(_) => None,
    }
}

impl Report {
    /// Which event this report is.
    pub fn event_name(&self) -> &'static str {
        match self.reading {
            Reading::Sensors(_) => EVENT_RECEIVED,
            Reading::Undecodable(_) => EVENT_UNDECODABLE,
        }
    }

    /// The event line's fields, in order: who, then every sensor, then the
    /// bytes. `fields_hex` is last because it is the long one.
    pub fn event_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![
            ("src", hex_encode(&self.source)),
            ("via", hex_encode(&self.via)),
        ];
        match &self.reading {
            Reading::Sensors(telemetry) => {
                for column in columns(telemetry) {
                    fields.push((
                        column.key,
                        column.value.unwrap_or_else(|| "none".to_string()),
                    ));
                }
            }
            Reading::Undecodable(detail) => {
                fields.push(("detail", crate::protocol::detail(detail)));
            }
        }
        fields.push(("fields_hex", hex_encode(&self.raw)));
        fields
    }

    /// The row this report appends to [`LOG_FILE`].
    ///
    /// Hand-written rather than serde: the crate has no JSON dependency, and
    /// every string value here is either hex, one of this module's own
    /// literals, or a decoder message — all of which go through
    /// [`json_string`], so the row is valid JSON whatever the decoder said.
    ///
    /// `received_at` is passed in rather than read from the clock so the
    /// decision and the timestamp are separable in a test
    /// (`leviculum-cli/src/lntd_store.rs`, `ingest`, for the same reason).
    pub fn json_row(&self, received_at: f64, message_timestamp: f64) -> String {
        let mut row = String::from("{");
        let _ = write!(row, "\"src\":{}", json_string(&hex_encode(&self.source)));
        let _ = write!(row, ",\"via\":{}", json_string(&hex_encode(&self.via)));
        let _ = write!(row, ",\"received_at\":{received_at:?}");
        let _ = write!(row, ",\"message_timestamp\":{message_timestamp:?}");
        match &self.reading {
            Reading::Sensors(telemetry) => {
                let _ = write!(row, ",\"status\":\"ok\"");
                for column in columns(telemetry) {
                    let _ = write!(
                        row,
                        ",{}:{}",
                        json_string(column.key),
                        column.value.as_deref().unwrap_or("null")
                    );
                }
                let _ = write!(
                    row,
                    ",\"producers\":{}",
                    match telemetry.power_production.as_deref() {
                        Some(producers) => json_string(&describe_producers(producers)),
                        None => "null".to_string(),
                    }
                );
            }
            Reading::Undecodable(detail) => {
                let _ = write!(
                    row,
                    ",\"status\":\"unreadable\",\"detail\":{}",
                    json_string(detail)
                );
            }
        }
        let _ = write!(row, ",\"raw\":{}}}", json_string(&hex_encode(&self.raw)));
        row
    }
}

/// One JSON string literal, quotes included, with the escapes RFC 8259
/// requires: the two mandatory ones and every control character.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Unix seconds now, as the row's `received_at`.
///
/// A clock before the epoch yields 0 rather than a panic: an unusable
/// timestamp on a row we keep is strictly better than losing the reading,
/// and the row's sensor `time` comes from the producer anyway.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or(0.0)
}

/// The append-only log at `<LXMF_STORAGE>/`[`LOG_FILE`].
///
/// Owned by whoever drains [`Out`](crate::Out) — the writer thread in
/// `main.rs`, the harness in a test — and never by a hook: the hooks run
/// under the core mutex and do no I/O at all (`processor`, "The rule this
/// file is written against"). The row crosses the same unbounded queue as
/// every other line the helper produces, so an fsync can never delay a
/// tick.
pub struct TelemetryLog {
    path: PathBuf,
}

impl TelemetryLog {
    pub fn new(storage_dir: &Path) -> Self {
        Self {
            path: storage_dir.join(LOG_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one row, as one line.
    ///
    /// Opened per row on purpose, rather than held: a walk emits a reading
    /// every few tens of seconds, so the open costs nothing, and it is what
    /// makes the file survive being rotated or moved out from under a
    /// running helper — a held descriptor would keep writing into the
    /// unlinked inode. One `write_all` of line plus newline, so a row is
    /// never torn.
    pub fn append(&self, row: &str) -> std::io::Result<()> {
        let mut line = String::with_capacity(row.len() + 1);
        line.push_str(row);
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::Identity;
    use leviculum_lxmf::telemetry::{build_report, Battery, Location, StreamEntry};
    use leviculum_lxmf::{constants, DeliveryMethod};

    /// The reading the instruction names: location, battery and time.
    pub(crate) fn walk_reading() -> Telemetry {
        Telemetry {
            time: Some(1_790_000_000),
            location: Some(Location {
                latitude_e6: 52_520_008,
                longitude_e6: 13_404_954,
                altitude_e2: 3_412,
                speed_e2: 137,
                bearing_e2: 9_150,
                accuracy_e2: 480,
                last_update: 1_789_999_995,
            }),
            battery: Some(Battery {
                charge_percent: Number::Int(87),
                charging: Some(false),
                temperature: None,
            }),
            ..Telemetry::default()
        }
    }

    pub(crate) fn source() -> Identity {
        Identity::generate(&mut rand_core::OsRng)
    }

    /// A reporting message, built by the producer's own path and then put
    /// through the wire encoding, so what is decoded here is what a
    /// receiver actually holds.
    pub(crate) fn received_report(telemetry: &Telemetry) -> Message {
        let identity = source();
        let destination = [0x11u8; 16];
        let message = build_report(
            destination,
            [0x22; 16],
            &identity,
            1_790_000_001.5,
            telemetry,
        )
        .expect("a reading with sensors builds a report");
        let on_air = message.on_air().expect("a signed report packs");
        Message::unpack(
            &on_air,
            Some(destination),
            Some(&identity),
            DeliveryMethod::Opportunistic,
        )
        .expect("our own report unpacks")
    }

    fn field<'a>(fields: &'a [(&'static str, String)], key: &str) -> &'a str {
        fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("no {key} among {fields:?}"))
    }

    /// Scaled integers print exactly, sign and all — the property the whole
    /// `scaled` helper exists for.
    #[test]
    fn a_scaled_integer_prints_exactly() {
        assert_eq!(scaled(52_520_008, 6), "52.520008");
        assert_eq!(scaled(-1, 6), "-0.000001");
        assert_eq!(scaled(-13_404_954, 6), "-13.404954");
        assert_eq!(scaled(0, 2), "0.00");
        assert_eq!(scaled(3_412, 2), "34.12");
        assert_eq!(scaled(i64::from(i32::MIN), 2), "-21474836.48");
    }

    /// Every decoded sensor reaches the event line under its own key, and a
    /// sensor with no reading is `none` rather than a missing key.
    #[test]
    fn the_event_line_carries_the_walk_position() {
        let reading = walk_reading();
        let message = received_report(&reading);
        let reports = reports(&message);
        assert_eq!(reports.len(), 1, "one FIELD_TELEMETRY, one report");
        let report = &reports[0];

        assert_eq!(report.event_name(), EVENT_RECEIVED);
        let fields = report.event_fields();
        assert_eq!(field(&fields, "src"), hex_encode(&[0x22u8; 16]));
        assert_eq!(field(&fields, "via"), hex_encode(&[0x22u8; 16]));
        assert_eq!(field(&fields, "time"), "1790000000");
        assert_eq!(field(&fields, "lat"), "52.520008");
        assert_eq!(field(&fields, "lon"), "13.404954");
        assert_eq!(field(&fields, "alt"), "34.12");
        assert_eq!(field(&fields, "speed"), "1.37");
        assert_eq!(field(&fields, "bearing"), "91.50");
        assert_eq!(field(&fields, "accuracy"), "4.80");
        assert_eq!(field(&fields, "fix_time"), "1789999995");
        assert_eq!(field(&fields, "battery_pct"), "87");
        assert_eq!(field(&fields, "battery_charging"), "false");
        // Decoded today, absent in this reading: present as `none`.
        assert_eq!(field(&fields, "battery_temp_c"), "none");
        assert_eq!(field(&fields, "rssi"), "none");
        assert_eq!(field(&fields, "snr"), "none");
        assert_eq!(field(&fields, "link_q"), "none");
        assert_eq!(field(&fields, "temp_c"), "none");
        assert_eq!(field(&fields, "power_w"), "none");
        // The bytes, so a sensor this build cannot read is still recoverable.
        assert_eq!(field(&fields, "fields_hex"), hex_encode(&reading.encode()));

        // The driver's tokeniser splits on whitespace: no value may contain
        // any, or one field would silently become two.
        for (key, value) in &fields {
            assert!(
                !value.chars().any(char::is_whitespace),
                "{key} must be one token: {value:?}"
            );
        }
    }

    /// A blob that is not a Telemeter map keeps its bytes and says why.
    #[test]
    fn an_unreadable_blob_keeps_its_bytes() {
        let identity = source();
        // A `FIELD_TELEMETRY` whose bin payload is not a msgpack map at
        // all: a bare string where the Telemeter should be.
        let mut value = Vec::new();
        leviculum_lxmf::msgpack::bin(&mut value, &[0xa3, b'n', b'o', b'!']);
        let message = Message::create(
            [0x11; 16],
            [0x33; 16],
            &identity,
            1_790_000_002.0,
            Vec::new(),
            Vec::new(),
            vec![(constants::FIELD_TELEMETRY, value)],
            DeliveryMethod::Opportunistic,
        )
        .expect("a message with one field builds");

        let reports = reports(&message);
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_eq!(report.event_name(), EVENT_UNDECODABLE);
        let fields = report.event_fields();
        assert_eq!(field(&fields, "src"), hex_encode(&[0x33u8; 16]));
        assert_eq!(field(&fields, "fields_hex"), "a36e6f21");
        assert!(
            field(&fields, "detail").contains("not_a_Telemeter_map"),
            "the detail must name the failure: {:?}",
            field(&fields, "detail")
        );

        let row = report.json_row(1_790_000_003.0, 1_790_000_002.0);
        assert!(row.contains("\"status\":\"unreadable\""), "{row}");
        assert!(row.contains("\"raw\":\"a36e6f21\""), "{row}");
    }

    /// The row carries the same values as the line, plus the two clocks —
    /// and it is one line, so the file stays `jsonl`.
    #[test]
    fn the_row_carries_the_same_reading() {
        let reading = walk_reading();
        let message = received_report(&reading);
        let report = &reports(&message)[0];
        let row = report.json_row(1_790_000_009.25, message.timestamp);

        assert!(!row.contains('\n'), "a row is one line: {row}");
        for expected in [
            "\"src\":\"22222222222222222222222222222222\"",
            "\"via\":\"22222222222222222222222222222222\"",
            "\"received_at\":1790000009.25",
            "\"message_timestamp\":1790000001.5",
            "\"status\":\"ok\"",
            "\"time\":1790000000",
            "\"lat\":52.520008",
            "\"lon\":13.404954",
            "\"alt\":34.12",
            "\"speed\":1.37",
            "\"bearing\":91.50",
            "\"accuracy\":4.80",
            "\"fix_time\":1789999995",
            "\"battery_pct\":87",
            "\"battery_charging\":false",
            "\"battery_temp_c\":null",
            "\"power_w\":null",
            "\"producers\":null",
        ] {
            assert!(row.contains(expected), "row lacks {expected}: {row}");
        }
        assert!(
            row.contains(&format!("\"raw\":\"{}\"", hex_encode(&reading.encode()))),
            "{row}"
        );
    }

    /// A collector's stream: one report per row, each attributed to the node
    /// that measured it rather than to the collector that relayed it.
    #[test]
    fn a_stream_row_keeps_its_own_source() {
        let identity = source();
        let reading = walk_reading();
        let value = leviculum_lxmf::telemetry::encode_stream_field_value(&[StreamEntry {
            source: vec![0xab; 16],
            timestamp: Number::Int(1_790_000_000),
            telemetry: reading.encode(),
            appearance: None,
        }]);
        let message = Message::create(
            [0x11; 16],
            [0x44; 16],
            &identity,
            1_790_000_004.0,
            Vec::new(),
            Vec::new(),
            vec![(constants::FIELD_TELEMETRY_STREAM, value)],
            DeliveryMethod::Opportunistic,
        )
        .expect("a message with one field builds");

        let reports = reports(&message);
        assert_eq!(reports.len(), 1);
        let fields = reports[0].event_fields();
        assert_eq!(field(&fields, "src"), hex_encode(&[0xabu8; 16]));
        assert_eq!(field(&fields, "via"), hex_encode(&[0x44u8; 16]));
        assert_eq!(field(&fields, "lat"), "52.520008");
    }

    /// An ordinary chat message contributes nothing, so nothing is emitted
    /// and nothing is appended.
    #[test]
    fn a_message_without_telemetry_reports_nothing() {
        let identity = source();
        let message = Message::create(
            [0x11; 16],
            [0x55; 16],
            &identity,
            1_790_000_005.0,
            Vec::new(),
            b"hello".to_vec(),
            Vec::new(),
            DeliveryMethod::Opportunistic,
        )
        .expect("a plain message builds");
        assert!(reports(&message).is_empty());
    }

    /// The log appends rather than truncates, and every row is its own line.
    #[test]
    fn the_log_appends_one_line_per_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = TelemetryLog::new(dir.path());
        assert_eq!(log.path(), dir.path().join(LOG_FILE));

        log.append("{\"a\":1}").expect("first row");
        log.append("{\"a\":2}").expect("second row");
        let text = std::fs::read_to_string(log.path()).expect("the log exists");
        assert_eq!(text, "{\"a\":1}\n{\"a\":2}\n");
    }

    /// A decoder message with whitespace or quotes in it must not be able to
    /// break either sink's grammar.
    #[test]
    fn a_detail_cannot_break_the_grammar() {
        let report = Report {
            source: vec![0x01; 16],
            via: [0x01; 16],
            raw: vec![0xff],
            reading: Reading::Undecodable("a \"quoted\"\nreason\ttyped".to_string()),
        };
        let fields = report.event_fields();
        assert_eq!(field(&fields, "detail"), "a_\"quoted\"_reason_typed");
        let row = report.json_row(1.0, 2.0);
        assert!(row.contains("\\\"quoted\\\""), "{row}");
        assert!(row.contains("\\n"), "{row}");
        assert!(row.contains("\\t"), "{row}");
        assert!(!row.contains('\n'), "a row is one line: {row}");
    }
}
