//! What `lntd` keeps, and how a received message becomes a row.
//!
//! Two halves, both reachable without a radio and both tested that way:
//! [`ingest`] turns one delivered LXMF [`Message`] into the row it deserves,
//! and [`Store`] is the SQLite file that row goes into.
//!
//! # The rule the whole file exists for
//!
//! **The raw blob is written always.** Sideband's sensor set grows, and the
//! decoder in `leviculum-lxmf` skips a sensor ID it has no arm for, exactly
//! as `Telemeter.from_packed` does. That tolerance is right for a codec and
//! wrong for an archive: a field test that silently drops the one field we
//! had not implemented yet has nothing to go back to. So every row carries
//! the bytes as they arrived beside the columns we could fill, and a blob we
//! could not decode at all is a row with `status = 'unreadable'` rather than
//! a message on the floor.
//!
//! The one thing that produces no row is an **empty** Telemeter map: zero
//! entries, nothing measured, nothing to archive. Note that this is counted
//! on the map and not on the decode result — a map carrying only sensors we
//! do not know decodes to an empty [`Telemetry`] and must still be kept.

use std::path::{Path, PathBuf};

use leviculum_lxmf::constants::FIELD_TELEMETRY;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::telemetry::{PowerProducer, Telemetry};
use leviculum_lxmf::Message;
use rusqlite::{Connection, OptionalExtension};

/// The schema this build writes.
///
/// Bumped whenever a column is added; [`Store::open`] migrates an older file
/// forward in place and refuses a newer one rather than writing rows a
/// future reader would mis-read. The version lives in SQLite's own
/// `PRAGMA user_version`, so `sqlite3 telemetry.db 'PRAGMA user_version'`
/// answers it without this binary.
pub const SCHEMA_VERSION: i64 = 1;

/// `status` for a row whose blob decoded.
pub const STATUS_OK: &str = "ok";
/// `status` for a row whose blob did not, kept for the blob alone.
pub const STATUS_UNREADABLE: &str = "unreadable";

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS reports (
    id                   INTEGER PRIMARY KEY,
    -- The LXMF message id. UNIQUE is what makes a restart across a write
    -- idempotent: the same report delivered twice is one row.
    message_id           BLOB    NOT NULL UNIQUE,
    source_hash          BLOB    NOT NULL,
    received_at          REAL    NOT NULL,
    message_timestamp    REAL    NOT NULL,
    status               TEXT    NOT NULL,
    detail               TEXT,
    raw                  BLOB    NOT NULL,
    sensor_time          INTEGER,
    latitude_e6          INTEGER,
    longitude_e6         INTEGER,
    altitude_e2          INTEGER,
    speed_e2             INTEGER,
    bearing_e2           INTEGER,
    accuracy_e2          INTEGER,
    location_last_update INTEGER,
    battery_percent      REAL,
    battery_charging     INTEGER,
    battery_temperature  REAL,
    link_rssi            REAL,
    link_snr             REAL,
    link_quality         REAL,
    temperature_c        REAL,
    power_production_w   REAL,
    power_producers      TEXT
);
CREATE INDEX IF NOT EXISTS reports_by_source_time
    ON reports (source_hash, received_at);
";

const INSERT: &str = "
INSERT OR IGNORE INTO reports (
    message_id, source_hash, received_at, message_timestamp, status, detail, raw,
    sensor_time,
    latitude_e6, longitude_e6, altitude_e2, speed_e2, bearing_e2, accuracy_e2,
    location_last_update,
    battery_percent, battery_charging, battery_temperature,
    link_rssi, link_snr, link_quality,
    temperature_c,
    power_production_w, power_producers
) VALUES (
    ?1, ?2, ?3, ?4, ?5, ?6, ?7,
    ?8,
    ?9, ?10, ?11, ?12, ?13, ?14,
    ?15,
    ?16, ?17, ?18,
    ?19, ?20, ?21,
    ?22,
    ?23, ?24
)";

/// Why the store could not be used. Every variant names the file: the
/// operator's next move is always to look at it.
#[derive(Debug)]
pub enum StoreError {
    Sqlite(PathBuf, rusqlite::Error),
    /// The directory the file lives in could not be made.
    Io(PathBuf, std::io::Error),
    /// The file was written by a later `lntd` than this one. Refused rather
    /// than migrated backwards — the columns this build does not know about
    /// are exactly the ones it would lose.
    FromTheFuture {
        path: PathBuf,
        version: i64,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::FromTheFuture { path, version } => write!(
                f,
                "{} carries schema version {version}, newer than the {SCHEMA_VERSION} \
                 this build writes.\n  \
                 Run the newer lntd against it, or point --database at a fresh file; \
                 it is not downgraded automatically, because the columns this build \
                 does not know about are the ones that would be lost.",
                path.display()
            ),
        }
    }
}

impl std::error::Error for StoreError {}

/// What a decoded report contributed, or why it contributed nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    /// The blob is a Telemeter map. Every sensor we understood is in here;
    /// the ones we did not are in the row's raw blob and nowhere else.
    Sensors(Box<Telemetry>),
    /// The blob is not one. The row is kept for the blob, with this as
    /// `detail`.
    Unreadable(String),
}

/// One row, before it is a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The LXMF message id: the dedup key, so a redelivery is not a second
    /// row.
    pub message_id: [u8; 32],
    /// Which node reported. The LXMF source destination hash — the same 16
    /// bytes an operator sees in a board's `[TELEMETRY]` line.
    pub source_hash: [u8; 16],
    /// Unix seconds, our clock, when the message reached this daemon.
    pub received_at: f64,
    /// Unix seconds, the sender's clock, from the LXMF message itself.
    /// Distinct from `sensor_time` (`SID_TIME`), which the reporter stamps
    /// on the reading rather than on the message.
    pub message_timestamp: f64,
    /// The Telemeter blob exactly as it arrived.
    pub raw: Vec<u8>,
    pub decoded: Decoded,
}

/// What [`ingest`] made of one delivered message.
#[derive(Debug, Clone, PartialEq)]
pub enum Ingest {
    /// Keep this.
    Row(Box<Report>),
    /// A well-formed Telemeter map with no entries: the sender measured
    /// nothing. The producer rule is not to send this at all, so it is a
    /// peer's slip rather than ours — noted, not stored, not fatal.
    EmptyMap,
    /// The message carries no `FIELD_TELEMETRY`; it is somebody's chat
    /// message to an address that only collects.
    NotTelemetry,
}

/// Turn one delivered LXMF message into the row it deserves.
///
/// `received_at` is passed in rather than read from the clock so the
/// decision and the timestamp are separable in a test.
pub fn ingest(message: &Message, received_at: f64) -> Ingest {
    let Some((_, value)) = message.fields.iter().find(|(id, _)| *id == FIELD_TELEMETRY) else {
        return Ingest::NotTelemetry;
    };

    // Sideband stores the packed bytes in the field, so the value is a
    // msgpack bin wrapping the blob (`Telemetry::encode_field_value`). A
    // value that is not even that is still kept — as itself, since there is
    // no inner blob to unwrap.
    let mut p = 0;
    let (blob, unwrap_failure) = match msgpack::read_bin(value, &mut p) {
        Ok(blob) => (blob.to_vec(), None),
        Err(e) => (
            value.clone(),
            Some(format!("FIELD_TELEMETRY is not a msgpack bin: {e:?}")),
        ),
    };

    let build = |decoded| {
        Ingest::Row(Box::new(Report {
            message_id: message.message_id,
            source_hash: message.source_hash,
            received_at,
            message_timestamp: message.timestamp,
            raw: blob.clone(),
            decoded,
        }))
    };

    if let Some(detail) = unwrap_failure {
        return build(Decoded::Unreadable(detail));
    }

    // Counted on the map, not on the decode result: a map whose every SID is
    // one we have no arm for decodes to an empty `Telemetry`, and that is the
    // case this daemon exists to not lose.
    let mut p = 0;
    let entries = match msgpack::map_len(&blob, &mut p) {
        Ok(entries) => entries,
        Err(e) => return build(Decoded::Unreadable(format!("not a Telemeter map: {e:?}"))),
    };
    if entries == 0 {
        return Ingest::EmptyMap;
    }

    match Telemetry::decode(&blob) {
        Ok(telemetry) => build(Decoded::Sensors(Box::new(telemetry))),
        Err(e) => build(Decoded::Unreadable(format!(
            "malformed Telemeter map: {e:?}"
        ))),
    }
}

/// The SQLite file, and the only thing that writes to it.
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (or create) the database at `path` and bring its schema to
    /// [`SCHEMA_VERSION`].
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Io(parent.to_path_buf(), e))?;
        }
        let conn = Connection::open(path).map_err(|e| StoreError::Sqlite(path.to_path_buf(), e))?;
        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.configure()?;
        store.migrate()?;
        Ok(store)
    }

    /// The pragmas that make "lose nothing" true rather than aspirational.
    ///
    /// WAL so a reader — the operator's own `sqlite3` session, which is the
    /// whole visualisation story — never blocks the writer, and
    /// `synchronous = FULL` so a row that returned is on the platter. The
    /// cost of FULL is one fsync per report; reports arrive minutes apart,
    /// so it buys durability against a power cut for nothing measurable.
    /// This runs on a field node whose power is the experiment.
    fn configure(&self) -> Result<(), StoreError> {
        self.conn
            .pragma_update(None, "journal_mode", "WAL")
            .and_then(|()| self.conn.pragma_update(None, "synchronous", "FULL"))
            .map_err(|e| self.err(e))
    }

    fn migrate(&self) -> Result<(), StoreError> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|e| self.err(e))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::FromTheFuture {
                path: self.path.clone(),
                version,
            });
        }
        if version < 1 {
            self.conn
                .execute_batch(SCHEMA_V1)
                .map_err(|e| self.err(e))?;
        }
        // A version 2 adds its columns here with `ALTER TABLE … ADD COLUMN`,
        // which SQLite does in place: the rows already written keep their
        // values and read NULL in the new column.
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| self.err(e))
    }

    /// The schema version of the open file.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        self.conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|e| self.err(e))
    }

    /// Write one report. `Ok(false)` means the message id was already there
    /// — a redelivery, not an error.
    pub fn insert(&self, report: &Report) -> Result<bool, StoreError> {
        let (status, detail, telemetry) = match &report.decoded {
            Decoded::Sensors(telemetry) => (STATUS_OK, None, Some(telemetry.as_ref())),
            Decoded::Unreadable(detail) => (STATUS_UNREADABLE, Some(detail.as_str()), None),
        };
        let location = telemetry.and_then(|t| t.location);
        let battery = telemetry.and_then(|t| t.battery);
        let link = telemetry.and_then(|t| t.physical_link);
        let producers = telemetry.and_then(|t| t.power_production.as_deref());

        let changed = self
            .conn
            .execute(
                INSERT,
                rusqlite::params![
                    report.message_id.as_slice(),
                    report.source_hash.as_slice(),
                    report.received_at,
                    report.message_timestamp,
                    status,
                    detail,
                    report.raw.as_slice(),
                    telemetry.and_then(|t| t.time),
                    location.map(|l| l.latitude_e6),
                    location.map(|l| l.longitude_e6),
                    location.map(|l| l.altitude_e2),
                    location.map(|l| l.speed_e2),
                    location.map(|l| l.bearing_e2),
                    location.map(|l| l.accuracy_e2),
                    location.map(|l| l.last_update),
                    battery.map(|b| b.charge_percent.as_f64()),
                    battery.and_then(|b| b.charging),
                    battery.and_then(|b| b.temperature).map(|t| t.as_f64()),
                    link.and_then(|l| l.rssi).map(|n| n.as_f64()),
                    link.and_then(|l| l.snr).map(|n| n.as_f64()),
                    link.and_then(|l| l.q).map(|n| n.as_f64()),
                    telemetry.and_then(|t| t.temperature).map(|n| n.as_f64()),
                    producers.map(total_watts),
                    producers.map(describe_producers),
                ],
            )
            .map_err(|e| self.err(e))?;
        Ok(changed == 1)
    }

    /// How many rows the file holds. For the startup line and for tests.
    pub fn row_count(&self) -> Result<i64, StoreError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
            .map_err(|e| self.err(e))
    }

    /// The newest `received_at`, or `None` while the file is empty.
    pub fn last_received_at(&self) -> Result<Option<f64>, StoreError> {
        self.conn
            .query_row("SELECT MAX(received_at) FROM reports", [], |row| row.get(0))
            .optional()
            .map(Option::flatten)
            .map_err(|e| self.err(e))
    }

    fn err(&self, error: rusqlite::Error) -> StoreError {
        StoreError::Sqlite(self.path.clone(), error)
    }
}

/// The curve column: watts across every producer in the reading.
///
/// A Solar Node reports one producer, so this is that producer's output;
/// the split across several stays in `power_producers` and, whole, in the
/// raw blob.
fn total_watts(producers: &[PowerProducer]) -> f64 {
    producers.iter().map(|p| p.power.as_f64()).sum()
}

/// The same reading spelled for an operator: `label=watts`, `;`-joined,
/// with the reference's unnamed default slot written as `default`.
fn describe_producers(producers: &[PowerProducer]) -> String {
    producers
        .iter()
        .map(|p| {
            let label = p.type_label.as_deref().unwrap_or("default");
            format!("{label}={}", p.power.as_f64())
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::msgpack::Number;
    use leviculum_lxmf::telemetry::{Battery, Location, PhysicalLink};
    use leviculum_lxmf::DeliveryMethod;

    /// A delivered message carrying `value` as `FIELD_TELEMETRY`, signed the
    /// way a board signs one — the ingest path is fed what the wire hands it,
    /// not a hand-built struct.
    fn telemetry_message(value: Vec<u8>) -> Message {
        telemetry_message_at(value, 1_700_000_000.5)
    }

    /// The reporter's own clock reading. It is part of what the message id
    /// hashes, so two reports are two rows only if they differ in *something*
    /// — which is the dedup rule working, not a test artefact.
    fn telemetry_message_at(value: Vec<u8>, timestamp: f64) -> Message {
        message_with_fields(vec![(FIELD_TELEMETRY, value)], timestamp)
    }

    fn message_with_fields(fields: Vec<(i64, Vec<u8>)>, timestamp: f64) -> Message {
        let identity = leviculum_std::generate_identity();
        let source_hash = [0x11u8; 16];
        Message::create(
            [0x22u8; 16],
            source_hash,
            &identity,
            timestamp,
            Vec::new(),
            Vec::new(),
            fields,
            DeliveryMethod::Direct,
        )
        .expect("a well-formed message")
    }

    fn a_full_telemetry() -> Telemetry {
        Telemetry {
            time: Some(1_700_000_123),
            location: Some(Location::saturating(
                52_520_008,
                13_404_954,
                3_400,
                250,
                18_000,
                500,
                1_700_000_100,
            )),
            battery: Some(Battery {
                charge_percent: Number::Int(87),
                charging: Some(true),
                temperature: Some(Number::Float(21.5)),
            }),
            physical_link: Some(PhysicalLink {
                rssi: Some(Number::Int(-92)),
                snr: Some(Number::Float(7.25)),
                q: Some(Number::Int(64)),
            }),
            temperature: Some(Number::Float(19.25)),
            power_production: Some(vec![
                PowerProducer {
                    type_label: Some("panel".into()),
                    power: Number::Float(4.5),
                    custom_icon: None,
                },
                PowerProducer {
                    type_label: None,
                    power: Number::Float(0.5),
                    custom_icon: None,
                },
            ]),
        }
    }

    fn row(store: &Store) -> Report {
        // Only ever called where exactly one row is expected.
        store
            .conn
            .query_row(
                "SELECT message_id, source_hash, received_at, message_timestamp, status, \
                 detail, raw FROM reports",
                [],
                |r| {
                    let message_id: Vec<u8> = r.get(0)?;
                    let source_hash: Vec<u8> = r.get(1)?;
                    let status: String = r.get(4)?;
                    let detail: Option<String> = r.get(5)?;
                    Ok(Report {
                        message_id: message_id.try_into().unwrap_or([0; 32]),
                        source_hash: source_hash.try_into().unwrap_or([0; 16]),
                        received_at: r.get(2)?,
                        message_timestamp: r.get(3)?,
                        raw: r.get(6)?,
                        decoded: match (status.as_str(), detail) {
                            (STATUS_UNREADABLE, Some(detail)) => Decoded::Unreadable(detail),
                            _ => Decoded::Sensors(Box::default()),
                        },
                    })
                },
            )
            .expect("exactly one row")
    }

    fn open_in(dir: &tempfile::TempDir) -> Store {
        Store::open(&dir.path().join("telemetry.db")).expect("open")
    }

    fn store_one(store: &Store, message: &Message, at: f64) -> bool {
        match ingest(message, at) {
            Ingest::Row(report) => store.insert(&report).expect("insert"),
            other => panic!("expected a row, got {other:?}"),
        }
    }

    /// The first proof: a blob built by our own encoder lands sensor by
    /// sensor in the columns named after those sensors.
    #[test]
    fn every_decoded_sensor_lands_in_its_own_column() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_in(&dir);
        let telemetry = a_full_telemetry();
        let message = telemetry_message(telemetry.encode_field_value());

        assert!(store_one(&store, &message, 1_700_000_200.0));

        let (status, sensor_time, lat, lon, alt, speed, bearing, accuracy, last_update): (
            String,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = store
            .conn
            .query_row(
                "SELECT status, sensor_time, latitude_e6, longitude_e6, altitude_e2, \
                 speed_e2, bearing_e2, accuracy_e2, location_last_update FROM reports",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .expect("one row");
        assert_eq!(status, STATUS_OK);
        assert_eq!(sensor_time, 1_700_000_123);
        assert_eq!(
            (lat, lon, alt, speed, bearing, accuracy, last_update),
            (
                52_520_008,
                13_404_954,
                3_400,
                250,
                18_000,
                500,
                1_700_000_100
            )
        );

        let (percent, charging, battery_temp, rssi, snr, q, temp, watts, producers): (
            f64,
            i64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
            String,
        ) = store
            .conn
            .query_row(
                "SELECT battery_percent, battery_charging, battery_temperature, \
                 link_rssi, link_snr, link_quality, temperature_c, \
                 power_production_w, power_producers FROM reports",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .expect("one row");
        assert_eq!(percent, 87.0);
        assert_eq!(charging, 1, "SQLite stores a bool as 1/0");
        assert_eq!(battery_temp, 21.5);
        assert_eq!((rssi, snr, q), (-92.0, 7.25, 64.0));
        assert_eq!(temp, 19.25);
        assert_eq!(watts, 5.0, "the curve column is the sum over producers");
        assert_eq!(producers, "panel=4.5; default=0.5");

        // And the blob is there beside the columns, always.
        let stored = row(&store);
        assert_eq!(stored.raw, telemetry.encode());
        assert_eq!(stored.source_hash, [0x11u8; 16]);
        assert_eq!(stored.received_at, 1_700_000_200.0);
        assert_eq!(stored.message_timestamp, 1_700_000_000.5);
    }

    /// The second proof, and the reason this daemon is worth running: a
    /// sensor we have never heard of still produces a row, and the bytes
    /// survive intact for whoever implements it later.
    #[test]
    fn an_unknown_sensor_still_produces_a_row_with_the_blob_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_in(&dir);

        // One entry, SID 0x7f, which no arm in `Telemetry::decode` matches.
        let mut blob = Vec::new();
        leviculum_lxmf::msgpack::map(&mut blob, 1);
        leviculum_lxmf::msgpack::int(&mut blob, 0x7f);
        leviculum_lxmf::msgpack::int(&mut blob, 4242);
        let mut value = Vec::new();
        leviculum_lxmf::msgpack::bin(&mut value, &blob);

        assert!(store_one(
            &store,
            &telemetry_message(value),
            1_700_000_300.0
        ));

        let stored = row(&store);
        assert_eq!(
            stored.raw, blob,
            "the unknown sensor's bytes are the only record of it"
        );
        let status: String = store
            .conn
            .query_row("SELECT status FROM reports", [], |r| r.get(0))
            .expect("one row");
        assert_eq!(
            status, STATUS_OK,
            "a map we could parse is 'ok' even when every sensor in it is new"
        );
        let sensor_time: Option<i64> = store
            .conn
            .query_row("SELECT sensor_time FROM reports", [], |r| r.get(0))
            .expect("one row");
        assert_eq!(
            sensor_time, None,
            "nothing was understood, so nothing is claimed"
        );
    }

    /// The third proof: the file is the state. Closing and reopening across
    /// a write loses no row, and redelivering a message adds none.
    #[test]
    fn a_restart_across_a_write_loses_nothing_and_doubles_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("telemetry.db");
        let first = telemetry_message(a_full_telemetry().encode_field_value());
        let second = telemetry_message_at(a_full_telemetry().encode_field_value(), 1_700_000_060.0);
        assert_ne!(
            first.message_id, second.message_id,
            "two reports must be two ids for this test to mean anything"
        );

        {
            let store = Store::open(&path).expect("first run");
            assert!(store_one(&store, &first, 1.0));
            assert_eq!(store.row_count().expect("count"), 1);
        }

        let store = Store::open(&path).expect("second run");
        assert_eq!(
            store.row_count().expect("count"),
            1,
            "the row written before the restart is still there"
        );
        assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
        assert!(
            !store_one(&store, &first, 2.0),
            "a redelivered message is not a second row"
        );
        assert_eq!(store.row_count().expect("count"), 1);
        assert!(store_one(&store, &second, 3.0));
        assert_eq!(store.row_count().expect("count"), 2);
        assert_eq!(store.last_received_at().expect("max"), Some(3.0));
        drop(store);

        let reopened = Store::open(&path).expect("third run");
        assert_eq!(reopened.row_count().expect("count"), 2);
    }

    /// The fourth proof: nothing measured is nothing stored, and no panic.
    #[test]
    fn an_empty_telemetry_map_produces_no_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_in(&dir);
        let message = telemetry_message(Telemetry::default().encode_field_value());

        assert_eq!(ingest(&message, 1.0), Ingest::EmptyMap);
        assert_eq!(store.row_count().expect("count"), 0);
    }

    /// The other half of "lose nothing": a blob we cannot read is a row that
    /// says so, with the bytes attached, not a message on the floor.
    #[test]
    fn an_unreadable_blob_is_recorded_as_such() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_in(&dir);
        let mut value = Vec::new();
        // A bin that is not a msgpack map at all.
        leviculum_lxmf::msgpack::bin(&mut value, b"\xc1garbage");

        assert!(store_one(&store, &telemetry_message(value), 9.0));

        let stored = row(&store);
        assert_eq!(stored.raw, b"\xc1garbage");
        match stored.decoded {
            Decoded::Unreadable(detail) => assert!(
                detail.contains("Telemeter map"),
                "the detail must say what failed: {detail}"
            ),
            other => panic!("expected an unreadable row, got {other:?}"),
        }
    }

    /// A field value that is not even the msgpack bin the format prescribes
    /// is still kept — as itself, since there is no inner blob to unwrap.
    #[test]
    fn a_field_value_that_is_not_a_bin_is_kept_as_it_arrived() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_in(&dir);
        let mut value = Vec::new();
        leviculum_lxmf::msgpack::int(&mut value, 7);

        assert!(store_one(&store, &telemetry_message(value.clone()), 11.0));
        assert_eq!(row(&store).raw, value);
    }

    #[test]
    fn a_message_without_a_telemetry_field_is_not_a_report() {
        let message = message_with_fields(Vec::new(), 1_700_000_000.5);
        assert_eq!(ingest(&message, 1.0), Ingest::NotTelemetry);
    }

    /// The migration seam: a file a newer build wrote is refused, not
    /// silently written into with this build's narrower schema.
    #[test]
    fn a_newer_schema_is_refused_rather_than_downgraded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("telemetry.db");
        {
            let store = Store::open(&path).expect("create");
            store
                .conn
                .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .expect("pretend a newer build wrote this");
        }
        match Store::open(&path) {
            Err(StoreError::FromTheFuture { version, .. }) => {
                assert_eq!(version, SCHEMA_VERSION + 1)
            }
            Ok(_) => panic!("a newer file must not be opened for writing"),
            Err(other) => panic!("wrong error: {other}"),
        }
    }

    /// The store creates the directory it was pointed at, so `--database`
    /// can name a path under a state directory that does not exist yet.
    #[test]
    fn opening_creates_the_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/deeper/telemetry.db");
        let store = Store::open(&path).expect("open");
        assert_eq!(store.row_count().expect("count"), 0);
        assert!(path.exists());
    }
}
