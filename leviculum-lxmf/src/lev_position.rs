//! `lev.position`: where a node is, in the announce it already sends
//! (order 1301, Refs #232).
//!
//! A board announces one more destination beside its `lxmf.delivery`: a
//! SINGLE destination of its own identity named `lev.position`, whose
//! announce `app_data` carries its fix. Every node in range hears it, so a
//! map shows the mesh's nodes without a mailbox and without a message.
//!
//! Its own aspect and never the delivery announce's `app_data`. The router
//! builds that one as a fixed msgpack list, `[display_name, stamp_cost,
//! [capabilities]]` (`reference/LXMF/LXMF/LXMRouter.py:1034-1050`), and every
//! reader parses it by that shape: `display_name_from_app_data`
//! (`reference/LXMF/LXMF/LXMF.py:151-172`) takes a payload whose first byte
//! is not a list marker for the pre-0.5 format and decodes the WHOLE of it
//! as UTF-8 (`:172`), so a map there is a `UnicodeDecodeError` or a name of
//! binary garbage on every Python and Sideband peer, and
//! `stamp_cost_from_app_data` (`:174-184`) reads element 1 of the list as a
//! stamp cost. A name hash Python has no handler for is a destination it
//! stores and relays and otherwise ignores.
//!
//! # The two forms
//!
//! The payload is a msgpack map (below). It goes on the air in one of two
//! forms, told apart by the first byte:
//!
//! * **sealed**, the default: `bin(Identity::encrypt(map))`, the map
//!   encrypted to the owner's identity, the same recipient the board's
//!   LXMF telemetry already goes to. An announce is plaintext, so a bare
//!   position would tell everyone in radio range where the board is; the
//!   seal leaves that to the owner's homes and devices, which hold the
//!   owner's key. Cost: 32 bytes of ephemeral key, a 16-byte IV, a 32-byte
//!   HMAC, the padding to the next 16-byte block, and the two-byte `bin8`
//!   header ([`sealed_len`]).
//! * **plain**, the configured opt-in "share my position with everyone
//!   nearby": the map itself.
//!
//! ```text
//! app_data = map {                    plain; sealed is bin(encrypt(this map))
//!     0x00: int    latitude,  degrees times 1e7
//!     0x01: int    longitude, degrees times 1e7
//!     0x02: int    altitude, metres above sea level        when the fix has one
//!     0x03: uint   ground speed, centimetres a second      when the fix has one
//!     0x04: uint   course over ground, degrees times 100   when the fix has one
//!     0x05: uint   age of the fix when the announce left, seconds
//!     0x06: uint   source: 0 GNSS, 1 manual (a fixed position someone set)
//!     0x07: str    the node's display name                 when it has one
//! }
//! ```
//!
//! The namespace's rule holds here as in the LXMF field pair
//! (`docs/src/concepts/loquax-read-receipts.md`, section 1): **a reader
//! skips a key it does not know, and a key never changes meaning.** A map
//! without both coordinates, or with one out of range, is no position. The
//! signature over the announce covers the `app_data`, so the position is
//! as authentic as the identity that announced it.

use crate::msgpack;
use crate::telemetry::Location;
use alloc::string::String;
use alloc::vec::Vec;
use leviculum_core::identity::Identity;
use leviculum_core::Destination;
use rand_core::CryptoRngCore;

/// The destination's app name, the namespace.
pub const APP_NAME: &str = "lev";
/// Its one aspect.
pub const ASPECT: &str = "position";

/// Map key: latitude, degrees times 1e7.
pub const KEY_LAT: i64 = 0x00;
/// Map key: longitude, degrees times 1e7.
pub const KEY_LON: i64 = 0x01;
/// Map key: altitude, metres.
pub const KEY_ALT: i64 = 0x02;
/// Map key: ground speed, centimetres a second.
pub const KEY_SPEED: i64 = 0x03;
/// Map key: course over ground, degrees times 100.
pub const KEY_HEADING: i64 = 0x04;
/// Map key: fix age, seconds.
pub const KEY_AGE: i64 = 0x05;
/// Map key: source.
pub const KEY_SOURCE: i64 = 0x06;
/// Map key: display name.
pub const KEY_NAME: i64 = 0x07;

/// The first byte of a sealed payload: `bin8`. A plain payload starts with
/// a map marker, so the two can never be confused.
const BIN8: u8 = 0xc4;

/// Where a position came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A satellite receiver's fix.
    Gnss,
    /// A position somebody set (`lnflash --fixed-position`).
    Manual,
}

impl Source {
    const fn wire(self) -> u64 {
        match self {
            Self::Gnss => 0,
            Self::Manual => 1,
        }
    }

    const fn from_wire(value: u64) -> Option<Self> {
        match value {
            0 => Some(Self::Gnss),
            1 => Some(Self::Manual),
            _ => None,
        }
    }
}

/// One node's position as its announce states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    /// Degrees north, times 1e7.
    pub lat_e7: i32,
    /// Degrees east, times 1e7.
    pub lon_e7: i32,
    /// Metres above sea level, when the fix has one.
    pub alt_m: Option<i32>,
    /// Ground speed in centimetres a second, when the fix has one.
    pub speed_cm_s: Option<u32>,
    /// Course over ground in degrees times 100, when the fix has one.
    pub heading_e2: Option<u16>,
    /// How old the fix was when the announce left, seconds.
    pub fix_age_s: u32,
    /// Where it came from; `None` when the map has no source, or one this
    /// reader does not know.
    pub source: Option<Source>,
    /// The node's display name, when it has one.
    pub name: Option<String>,
}

impl Position {
    /// The plain form: the map.
    pub fn encode(&self) -> Vec<u8> {
        let entries = 3
            + usize::from(self.alt_m.is_some())
            + usize::from(self.speed_cm_s.is_some())
            + usize::from(self.heading_e2.is_some())
            + usize::from(self.source.is_some())
            + usize::from(self.name.is_some());
        let mut out = Vec::new();
        msgpack::map(&mut out, entries);
        msgpack::int(&mut out, KEY_LAT);
        msgpack::int(&mut out, i64::from(self.lat_e7));
        msgpack::int(&mut out, KEY_LON);
        msgpack::int(&mut out, i64::from(self.lon_e7));
        if let Some(alt) = self.alt_m {
            msgpack::int(&mut out, KEY_ALT);
            msgpack::int(&mut out, i64::from(alt));
        }
        if let Some(speed) = self.speed_cm_s {
            msgpack::int(&mut out, KEY_SPEED);
            msgpack::uint(&mut out, u64::from(speed));
        }
        if let Some(heading) = self.heading_e2 {
            msgpack::int(&mut out, KEY_HEADING);
            msgpack::uint(&mut out, u64::from(heading));
        }
        msgpack::int(&mut out, KEY_AGE);
        msgpack::uint(&mut out, u64::from(self.fix_age_s));
        if let Some(source) = self.source {
            msgpack::int(&mut out, KEY_SOURCE);
            msgpack::uint(&mut out, source.wire());
        }
        if let Some(name) = &self.name {
            msgpack::int(&mut out, KEY_NAME);
            msgpack::string(&mut out, name);
        }
        out
    }

    /// Read the plain form. `None` for anything that is not a map with
    /// both coordinates in range; an unknown key is skipped, a known key
    /// of the wrong type makes the whole map no position, and a repeated
    /// key resolves to its last occurrence.
    pub fn decode(d: &[u8]) -> Option<Self> {
        let mut p = 0;
        let entries = msgpack::map_len(d, &mut p).ok()?;
        let mut lat = None;
        let mut lon = None;
        let mut position = Self {
            lat_e7: 0,
            lon_e7: 0,
            alt_m: None,
            speed_cm_s: None,
            heading_e2: None,
            fix_age_s: 0,
            source: None,
            name: None,
        };
        for _ in 0..entries {
            let key = msgpack::read_int(d, &mut p).ok();
            match key {
                Some(KEY_LAT) => {
                    lat = Some(i32::try_from(msgpack::read_int(d, &mut p).ok()?).ok()?)
                }
                Some(KEY_LON) => {
                    lon = Some(i32::try_from(msgpack::read_int(d, &mut p).ok()?).ok()?)
                }
                Some(KEY_ALT) => {
                    position.alt_m = Some(i32::try_from(msgpack::read_int(d, &mut p).ok()?).ok()?)
                }
                Some(KEY_SPEED) => {
                    position.speed_cm_s =
                        Some(u32::try_from(msgpack::read_uint(d, &mut p).ok()?).ok()?)
                }
                Some(KEY_HEADING) => {
                    position.heading_e2 =
                        Some(u16::try_from(msgpack::read_uint(d, &mut p).ok()?).ok()?)
                }
                Some(KEY_AGE) => {
                    position.fix_age_s = u32::try_from(msgpack::read_uint(d, &mut p).ok()?).ok()?
                }
                Some(KEY_SOURCE) => {
                    position.source = Source::from_wire(msgpack::read_uint(d, &mut p).ok()?)
                }
                Some(KEY_NAME) => position.name = Some(msgpack::read_str(d, &mut p).ok()?.into()),
                // A key of another type, or one this reader does not know.
                _ => {
                    if key.is_none() {
                        msgpack::skip(d, &mut p).ok()?;
                    }
                    msgpack::skip(d, &mut p).ok()?;
                }
            }
        }
        position.lat_e7 = lat.filter(|v| (-900_000_000..=900_000_000).contains(v))?;
        position.lon_e7 = lon.filter(|v| (-1_800_000_000..=1_800_000_000).contains(v))?;
        Some(position)
    }

    /// The sealed form: the map encrypted to `owner`, in a `bin`.
    pub fn seal<R: CryptoRngCore>(&self, owner: &Identity, rng: &mut R) -> Option<Vec<u8>> {
        let sealed = owner.encrypt(&self.encode(), rng).ok()?;
        let mut out = Vec::with_capacity(sealed.len() + 3);
        msgpack::bin(&mut out, &sealed);
        Some(out)
    }

    /// The position as LXMF telemetry carries one, for a reader that shows
    /// both on one map. `heard_at` is the unix second the announce
    /// arrived; the fix is [`fix_age_s`](Self::fix_age_s) older.
    pub fn location(&self, heard_at: i64) -> Location {
        Location {
            latitude_e6: self.lat_e7 / 10,
            longitude_e6: self.lon_e7 / 10,
            altitude_e2: self.alt_m.unwrap_or(0).saturating_mul(100),
            speed_e2: self.speed_cm_s.unwrap_or(0),
            bearing_e2: i32::from(self.heading_e2.unwrap_or(0)),
            accuracy_e2: 0,
            last_update: heard_at.saturating_sub(i64::from(self.fix_age_s)),
        }
    }
}

/// What a `lev.position` announce's `app_data` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppData<'a> {
    /// Readable by everyone.
    Plain(Position),
    /// Encrypted to one identity; the ciphertext.
    Sealed(&'a [u8]),
}

impl<'a> AppData<'a> {
    /// Tell the two forms apart. `None` for neither.
    pub fn parse(app_data: &'a [u8]) -> Option<Self> {
        if app_data.first() == Some(&BIN8) {
            let mut p = 0;
            let sealed = msgpack::read_bin(app_data, &mut p).ok()?;
            return (p == app_data.len()).then_some(Self::Sealed(sealed));
        }
        Position::decode(app_data).map(Self::Plain)
    }
}

/// The position in `app_data`, plain, or sealed to `own` (an identity
/// with its private key). `None` when there is none, or it is sealed to
/// somebody else.
pub fn open(app_data: &[u8], own: &Identity) -> Option<Position> {
    match AppData::parse(app_data)? {
        AppData::Plain(position) => Some(position),
        AppData::Sealed(ciphertext) => Position::decode(&own.decrypt(ciphertext).ok()?),
    }
}

/// The name hash a `lev.position` announce carries.
pub fn name_hash() -> [u8; 10] {
    Destination::compute_name_hash(APP_NAME, &[ASPECT])
}

/// The bytes the sealed form takes for a plain map of `plain_len` bytes:
/// the `bin8` header (2), the ephemeral key (32), the IV (16), the map
/// padded to the next whole 16-byte block (PKCS#7 always pads, so a map of
/// exactly 32 bytes takes 48), the HMAC (32).
pub const fn sealed_len(plain_len: usize) -> usize {
    2 + 32 + 16 + (plain_len / 16 + 1) * 16 + 32
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

    /// Florence, walking, with a fix two seconds old.
    fn walking() -> Position {
        Position {
            lat_e7: 437_731_000,
            lon_e7: 112_560_000,
            alt_m: Some(50),
            speed_cm_s: Some(140),
            heading_e2: Some(27_000),
            fix_age_s: 2,
            source: Some(Source::Gnss),
            name: Some("LNode-1a2b3c4d".into()),
        }
    }

    #[test]
    fn the_plain_map_round_trips_with_and_without_its_optional_keys() {
        let full = walking();
        assert_eq!(Position::decode(&full.encode()), Some(full.clone()));
        let bare = Position {
            alt_m: None,
            speed_cm_s: None,
            heading_e2: None,
            source: None,
            name: None,
            ..full
        };
        assert_eq!(Position::decode(&bare.encode()), Some(bare.clone()));
        let south_west = Position {
            lat_e7: -339_000_000,
            lon_e7: -1_799_999_999,
            alt_m: Some(-12),
            ..bare
        };
        assert_eq!(Position::decode(&south_west.encode()), Some(south_west));
    }

    #[test]
    fn the_map_costs_these_bytes() {
        // 1 map header, lat and lon 6 each (key + int32), alt 2 (key +
        // fixint 50), speed 3 (key + uint8 140), heading 4 (key + uint16),
        // age 2, source 2, name 16 (key + fixstr header + 14).
        assert_eq!(walking().encode().len(), 1 + 6 + 6 + 2 + 3 + 4 + 2 + 2 + 16);
        assert_eq!(walking().encode().len(), 42);
        let bare = Position {
            alt_m: None,
            speed_cm_s: None,
            heading_e2: None,
            source: None,
            name: None,
            ..walking()
        };
        assert_eq!(bare.encode().len(), 15);
    }

    #[test]
    fn a_sealed_position_opens_for_its_owner_and_for_nobody_else() {
        let owner = Identity::generate(&mut OsRng);
        let stranger = Identity::generate(&mut OsRng);
        let owner_public = Identity::from_public_key_bytes(&owner.public_key_bytes()).unwrap();
        let sealed = walking().seal(&owner_public, &mut OsRng).unwrap();
        assert_eq!(sealed.len(), sealed_len(walking().encode().len()));
        assert_eq!(sealed.len(), 130);
        assert!(matches!(AppData::parse(&sealed), Some(AppData::Sealed(_))));
        assert_eq!(open(&sealed, &owner), Some(walking()));
        assert_eq!(open(&sealed, &stranger), None);
        // The plain form opens for everybody.
        assert_eq!(open(&walking().encode(), &stranger), Some(walking()));
    }

    #[test]
    fn the_sealed_form_names_no_coordinate_in_the_clear() {
        let owner = Identity::generate(&mut OsRng);
        let sealed = walking().seal(&owner, &mut OsRng).unwrap();
        let lat = 437_731_000i32.to_be_bytes();
        assert!(!sealed.windows(4).any(|w| w == lat));
        assert!(!sealed.windows(5).any(|w| w == b"LNode"));
    }

    #[test]
    fn what_is_not_a_position_reads_as_none() {
        // Empty, another msgpack type, a list (the delivery announce's shape).
        assert_eq!(AppData::parse(&[]), None);
        assert_eq!(AppData::parse(&[0x01]), None);
        assert_eq!(
            AppData::parse(
                &crate::announce::DeliveryAnnounce {
                    display_name: Some(b"x".to_vec()),
                    stamp_cost: None,
                    compression_supported: false,
                }
                .encode()
            ),
            None
        );
        // 1302's stand-in layout: version byte 1, then raw integers.
        let mut stand_in = [0u8; 18];
        stand_in[0] = 1;
        assert_eq!(AppData::parse(&stand_in), None);
        // A map missing a coordinate, or with one out of range.
        let mut only_lat = Vec::new();
        msgpack::map(&mut only_lat, 1);
        msgpack::int(&mut only_lat, KEY_LAT);
        msgpack::int(&mut only_lat, 1);
        assert_eq!(Position::decode(&only_lat), None);
        let north_of_the_pole = Position {
            lat_e7: 900_000_001,
            ..walking()
        };
        assert_eq!(Position::decode(&north_of_the_pole.encode()), None);
        // A known key of the wrong type.
        let mut bad = Vec::new();
        msgpack::map(&mut bad, 3);
        msgpack::int(&mut bad, KEY_LAT);
        msgpack::int(&mut bad, 1);
        msgpack::int(&mut bad, KEY_LON);
        msgpack::int(&mut bad, 1);
        msgpack::int(&mut bad, KEY_NAME);
        msgpack::uint(&mut bad, 7);
        assert_eq!(Position::decode(&bad), None);
        // A sealed bin with trailing bytes.
        let mut trailing = Vec::new();
        msgpack::bin(&mut trailing, &[0u8; 96]);
        trailing.push(0);
        assert_eq!(AppData::parse(&trailing), None);
    }

    #[test]
    fn a_reader_skips_a_key_it_does_not_know() {
        let mut later = Vec::new();
        msgpack::map(&mut later, 4);
        msgpack::int(&mut later, 0x40);
        msgpack::array(&mut later, 2);
        msgpack::uint(&mut later, 1);
        msgpack::string(&mut later, "x");
        msgpack::int(&mut later, KEY_LAT);
        msgpack::int(&mut later, 10);
        msgpack::string(&mut later, "a string key");
        msgpack::uint(&mut later, 3);
        msgpack::int(&mut later, KEY_LON);
        msgpack::int(&mut later, 20);
        let position = Position::decode(&later).unwrap();
        assert_eq!((position.lat_e7, position.lon_e7), (10, 20));
        // An unknown source is skipped, not a refusal.
        let mut unknown_source = Vec::new();
        msgpack::map(&mut unknown_source, 3);
        msgpack::int(&mut unknown_source, KEY_LAT);
        msgpack::int(&mut unknown_source, 10);
        msgpack::int(&mut unknown_source, KEY_LON);
        msgpack::int(&mut unknown_source, 20);
        msgpack::int(&mut unknown_source, KEY_SOURCE);
        msgpack::uint(&mut unknown_source, 9);
        assert_eq!(Position::decode(&unknown_source).unwrap().source, None);
    }

    #[test]
    fn the_location_is_the_fix_at_its_age() {
        let location = walking().location(1_800_000_000);
        assert_eq!(location.latitude_e6, 43_773_100);
        assert_eq!(location.longitude_e6, 11_256_000);
        assert_eq!(location.altitude_e2, 5_000);
        assert_eq!(location.speed_e2, 140);
        assert_eq!(location.bearing_e2, 27_000);
        assert_eq!(location.last_update, 1_800_000_000 - 2);
    }

    #[test]
    fn the_name_hash_is_lev_position() {
        assert_eq!(
            name_hash(),
            Destination::compute_name_hash("lev", &["position"])
        );
    }
}
