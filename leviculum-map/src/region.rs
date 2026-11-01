//! The region file: one tile pyramid of `leviculum-mapfill`'s flat
//! geometry, read in place (`docs/src/concepts/loquax-map.md`, section 4,
//! "Region file"), from version 2 (LMR2) the names on it: one string
//! table and a label section per tile ([`crate::labels`]), and from
//! version 3 (LMR3) the buildings: a building section per z15 tile
//! ([`crate::buildings`]).
//!
//! All integers little-endian. Offsets are from the start of the file.
//!
//! ```text
//! header  64 bytes (version 1), 96 (version 2), 112 (version 3)
//!   0  "LMR1"
//!   4  u16 version (1 to 3)     6  u16 header length (64, 96 or 112)
//!   8  i32 west, south, east, north, degrees x 10^7 (west > east: the
//!      region crosses the antimeridian)
//!  24  u8 min zoom  25 u8 max zoom (<= 16)  26 u8 layer profile  27 u8 0
//!  28  u32 attribution offset   32 u32 attribution length
//!  36  u32 notice offset        40 u32 notice length
//!  44  u32 tile count           48 u64 index offset
//!  56  u64 file length
//!   version 2 and 3:
//!  64  u64 label index offset   72 u32 label section count
//!  76  u32 string count         80 u64 string table offset
//!  88  u64 string table length
//!   version 3 only:
//!  96  u64 building index offset  104 u32 building section count
//! 108  u32 0
//! text    the attribution (one line, shown on the map) and the notice
//!         (the licence and the source), UTF-8, both required
//! tiles   one `LMF1` fixture per tile: view 256x256, one tile at offset
//!         (0, 0), its shift taking the tile's extent to 4096 sub-pixels
//! labels  version 2: one label section per tile that has names
//! buildings  version 3: one building section per z15 tile that has
//!         buildings
//! strings version 2: the string table, after every blob and section
//! label index  version 2: label section count x 20 bytes, the entries'
//!         layout, ending where the building index starts (version 3)
//!         or the index (version 2)
//! building index  version 3: building section count x 20 bytes, the
//!         entries' layout, ending where the index starts
//! index   tile count x 20 bytes, ascending by (z, x, y), last in the file:
//!         u8 z  u8 0  u16 x  u16 y  u16 0  u64 offset  u32 length
//! ```
//!
//! A tile with nothing in it under the profile has no entry; a reader
//! draws the background there. Two entries may share one blob (the cut
//! stores identical small tiles once). The index sits last so the writer
//! can stream tiles and learn their count at the end, and it is searched
//! in place, 20 bytes per probe: a whole country's index is megabytes,
//! which the M9 does not load.
//!
//! A reader refuses a version newer than it knows with
//! [`RegionError::Version`] before it reads anything past the version
//! field, never misreads it: version 1 readers refuse version 2 and 3,
//! version 2 readers (those before 1191) refuse version 3. This reader reads
//! all three. A cut without buildings writes version 2, which every
//! version 2 reader still reads. A frontend that draws no names never
//! touches the label index or the string table, one that draws no
//! buildings never touches the building index, so they cost it nothing.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use leviculum_mapfill::fixture::{Fixture, FixtureError, Kind};

use crate::buildings::{BuildingError, Buildings};
use crate::labels::{LabelError, Section};

/// The file's first four bytes.
pub const MAGIC: [u8; 4] = *b"LMR1";
/// The format version the writer writes, the newest this reader reads:
/// names and buildings.
pub const VERSION: u16 = 3;
/// The version with names and without buildings, which a cut without
/// buildings writes and this reader reads too.
pub const VERSION_2: u16 = 2;
/// The first version, without names, which this reader reads too.
pub const VERSION_1: u16 = 1;
/// Header length in bytes of a version 1 file, the part every version
/// starts with.
pub const HEADER_LEN: usize = 64;
/// Header length in bytes of a version 2 file.
pub const HEADER_LEN_2: usize = 96;
/// Header length in bytes of a version 3 file.
pub const HEADER_LEN_3: usize = 112;
/// Index entry length in bytes.
pub const ENTRY_LEN: usize = 20;
/// The highest zoom a region may hold: x and y are `u16`.
pub const MAX_ZOOM: u8 = 16;
/// Longest attribution or notice a reader accepts.
pub const MAX_TEXT: u32 = 16 * 1024;
/// The view every tile blob is written for: one tile at 256 pixels.
pub const TILE_VIEW: u16 = 256;

/// Why bytes are not a region, or a tile in it is unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError {
    /// No `LMR1` at the start.
    Magic,
    /// A version this reader does not know.
    Version(u16),
    /// The file is shorter than its header says.
    Truncated,
    /// A header field is out of range; names the field.
    Header(&'static str),
    /// Index entry `n` is out of range or out of order.
    Entry(u32),
    /// The attribution or notice is not UTF-8, or is empty.
    Text,
    /// The storage could not be read.
    Io,
    /// A tile blob is not a fixture, or not a one-tile one.
    Tile(TileId, FixtureError),
    /// The string table is not one, or string `n` is not there.
    Strings(u32),
    /// A tile's label section is not one, or names a feature its tile
    /// does not hold as a line.
    Labels(TileId, LabelError),
    /// A tile's building section is not one.
    Buildings(TileId, BuildingError),
}

/// A tile address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    /// Whether x and y lie on zoom z's grid.
    pub fn is_valid(&self) -> bool {
        self.z <= MAX_ZOOM && self.x < (1 << self.z) && self.y < (1 << self.z)
    }
}

/// A bounding box in degrees x 10^7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub west: i32,
    pub south: i32,
    pub east: i32,
    pub north: i32,
}

/// The decoded header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub bounds: Bounds,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub profile: u8,
    /// Offset and length of the attribution.
    pub attribution: (u32, u32),
    /// Offset and length of the notice.
    pub notice: (u32, u32),
    pub tiles: u32,
    pub index: u64,
    pub len: u64,
    /// The names: `None` in a version 1 file, which has none.
    pub labels: Option<LabelTables>,
    /// The buildings: `None` before version 3. A version 3 header has
    /// names too; [`Header::encode`] writes empty ones where they are
    /// `None`.
    pub buildings: Option<BuildingTables>,
}

/// Where a version 3 file keeps its buildings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildingTables {
    /// The building index: entries like the tile index's, pointing at
    /// building sections.
    pub index: u64,
    pub sections: u32,
}

/// Where a version 2 file keeps its names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LabelTables {
    /// The label index: entries like the tile index's, pointing at label
    /// sections.
    pub index: u64,
    pub sections: u32,
    pub strings: u32,
    /// Offset and length of the string table.
    pub table: u64,
    pub table_len: u64,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

impl Header {
    /// The header's length in a file of `version`, `None` for a version
    /// this reader does not know.
    pub fn len_of(version: u16) -> Option<usize> {
        match version {
            VERSION_1 => Some(HEADER_LEN),
            VERSION_2 => Some(HEADER_LEN_2),
            VERSION => Some(HEADER_LEN_3),
            _ => None,
        }
    }

    /// The version this header encodes as: 3 with buildings, 2 with names
    /// alone, 1 with neither.
    pub fn version_of(&self) -> u16 {
        match (self.buildings, self.labels) {
            (Some(_), _) => VERSION,
            (None, Some(_)) => VERSION_2,
            (None, None) => VERSION_1,
        }
    }

    /// This header's length: 112 bytes with buildings, 96 with names
    /// alone, 64 with neither.
    pub fn header_len(&self) -> usize {
        match self.version_of() {
            VERSION => HEADER_LEN_3,
            VERSION_2 => HEADER_LEN_2,
            _ => HEADER_LEN,
        }
    }

    /// The version a file starting with these bytes says it is, after the
    /// magic is checked; [`RegionError::Version`] for one this reader does
    /// not know, before anything else of the header is read.
    pub fn version(b: &[u8; HEADER_LEN]) -> Result<u16, RegionError> {
        Self::version_up_to(b, VERSION)
    }

    /// [`Header::version`] for a reader whose newest version is `newest`:
    /// the check every reader runs first, so a test can hold an older
    /// reader's refusal to the rule.
    pub fn version_up_to(b: &[u8; HEADER_LEN], newest: u16) -> Result<u16, RegionError> {
        if b[..4] != MAGIC {
            return Err(RegionError::Magic);
        }
        let version = u16_at(b, 4);
        if version > newest {
            return Err(RegionError::Version(version));
        }
        Self::len_of(version).ok_or(RegionError::Version(version))?;
        Ok(version)
    }

    /// Decode and check the fields that need nothing but the header: the
    /// first 64 bytes, for version 2 the 32 after them, and for version 3
    /// the 16 after those. The checks
    /// against the file's length are [`Region::open`]'s.
    pub fn parse(b: &[u8]) -> Result<Self, RegionError> {
        let head: &[u8; HEADER_LEN] = b
            .get(..HEADER_LEN)
            .and_then(|h| h.try_into().ok())
            .ok_or(RegionError::Truncated)?;
        let version = Self::version(head)?;
        let len = Self::len_of(version).ok_or(RegionError::Version(version))?;
        if b.len() < len {
            return Err(RegionError::Truncated);
        }
        if usize::from(u16_at(b, 6)) != len {
            return Err(RegionError::Header("header length"));
        }
        let i = |at| u32_at(b, at) as i32;
        let bounds = Bounds {
            west: i(8),
            south: i(12),
            east: i(16),
            north: i(20),
        };
        let lon = -1_800_000_000..=1_800_000_000;
        let lat = -crate::mercator::MAX_LAT_E7..=crate::mercator::MAX_LAT_E7;
        if !lon.contains(&bounds.west)
            || !lon.contains(&bounds.east)
            || !lat.contains(&bounds.south)
            || !lat.contains(&bounds.north)
            || bounds.south >= bounds.north
            || bounds.west == bounds.east
        {
            return Err(RegionError::Header("bounds"));
        }
        let (min_zoom, max_zoom) = (b[24], b[25]);
        if min_zoom > max_zoom || max_zoom > MAX_ZOOM {
            return Err(RegionError::Header("zoom"));
        }
        if b[27] != 0 || (version == VERSION && u32_at(b, 108) != 0) {
            return Err(RegionError::Header("reserved"));
        }
        Ok(Self {
            bounds,
            min_zoom,
            max_zoom,
            profile: b[26],
            attribution: (u32_at(b, 28), u32_at(b, 32)),
            notice: (u32_at(b, 36), u32_at(b, 40)),
            tiles: u32_at(b, 44),
            index: u64_at(b, 48),
            len: u64_at(b, 56),
            labels: (version >= VERSION_2).then(|| LabelTables {
                index: u64_at(b, 64),
                sections: u32_at(b, 72),
                strings: u32_at(b, 76),
                table: u64_at(b, 80),
                table_len: u64_at(b, 88),
            }),
            buildings: (version == VERSION).then(|| BuildingTables {
                index: u64_at(b, 96),
                sections: u32_at(b, 104),
            }),
        })
    }

    /// The header bytes: version 3 with buildings, 2 with names alone, 1
    /// with neither ([`Header::version_of`]); the writer's half, kept
    /// beside the reader.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = vec![0u8; self.header_len()];
        let version = self.version_of();
        b[..4].copy_from_slice(&MAGIC);
        b[4..6].copy_from_slice(&version.to_le_bytes());
        b[6..8].copy_from_slice(&(self.header_len() as u16).to_le_bytes());
        let bd = self.bounds;
        for (at, v) in [(8, bd.west), (12, bd.south), (16, bd.east), (20, bd.north)] {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[24] = self.min_zoom;
        b[25] = self.max_zoom;
        b[26] = self.profile;
        for (at, v) in [
            (28, self.attribution.0),
            (32, self.attribution.1),
            (36, self.notice.0),
            (40, self.notice.1),
            (44, self.tiles),
        ] {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[48..56].copy_from_slice(&self.index.to_le_bytes());
        b[56..64].copy_from_slice(&self.len.to_le_bytes());
        if version >= VERSION_2 {
            let l = self.labels.unwrap_or_default();
            b[64..72].copy_from_slice(&l.index.to_le_bytes());
            b[72..76].copy_from_slice(&l.sections.to_le_bytes());
            b[76..80].copy_from_slice(&l.strings.to_le_bytes());
            b[80..88].copy_from_slice(&l.table.to_le_bytes());
            b[88..96].copy_from_slice(&l.table_len.to_le_bytes());
        }
        if let Some(bt) = self.buildings {
            b[96..104].copy_from_slice(&bt.index.to_le_bytes());
            b[104..108].copy_from_slice(&bt.sections.to_le_bytes());
        }
        b
    }
}

/// One index entry: a tile and where its blob is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: TileId,
    pub offset: u64,
    pub len: u32,
}

impl Entry {
    /// Decode an entry; `None` when a reserved byte is set.
    pub fn parse(b: &[u8; ENTRY_LEN]) -> Option<Self> {
        if b[1] != 0 || b[6] != 0 || b[7] != 0 {
            return None;
        }
        Some(Self {
            id: TileId {
                z: b[0],
                x: u32::from(u16_at(b, 2)),
                y: u32::from(u16_at(b, 4)),
            },
            offset: u64_at(b, 8),
            len: u32_at(b, 16),
        })
    }

    /// The 20 entry bytes. `id` must be valid ([`TileId::is_valid`]).
    pub fn encode(&self) -> [u8; ENTRY_LEN] {
        let mut b = [0u8; ENTRY_LEN];
        b[0] = self.id.z;
        b[2..4].copy_from_slice(&(self.id.x as u16).to_le_bytes());
        b[4..6].copy_from_slice(&(self.id.y as u16).to_le_bytes());
        b[8..16].copy_from_slice(&self.offset.to_le_bytes());
        b[16..20].copy_from_slice(&self.len.to_le_bytes());
        b
    }
}

/// Random-access bytes: a slice in flash or memory, a file, a card.
pub trait Storage {
    /// Length in bytes.
    fn size(&self) -> u64;
    /// Fill `buf` from `offset`; an error when the range is not there.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), RegionError>;
}

impl Storage for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), RegionError> {
        let start = usize::try_from(offset).map_err(|_| RegionError::Truncated)?;
        let src = start
            .checked_add(buf.len())
            .and_then(|end| self.get(start..end))
            .ok_or(RegionError::Truncated)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

#[cfg(all(feature = "std", unix))]
impl Storage for std::fs::File {
    fn size(&self) -> u64 {
        self.metadata().map(|m| m.len()).unwrap_or(0)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), RegionError> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset).map_err(|_| RegionError::Io)
    }
}

/// An opened region: the header checked against the storage, the two
/// texts read, the index left where it is.
pub struct Region<S> {
    storage: S,
    header: Header,
    attribution: String,
    notice: String,
    /// Where the blobs may lie: after the header, before the index.
    data_end: u64,
}

impl<S: Storage> Region<S> {
    /// Check the header against the storage and read the two texts.
    pub fn open(storage: S) -> Result<Self, RegionError> {
        let mut hb = [0u8; HEADER_LEN_3];
        if storage.size() < HEADER_LEN as u64 {
            // Too short for a header: say Magic when even that is wrong.
            let n = storage.size() as usize;
            let mut head = [0u8; 4];
            if n >= 4 {
                storage.read_at(0, &mut head)?;
                if head != MAGIC {
                    return Err(RegionError::Magic);
                }
            }
            return Err(RegionError::Truncated);
        }
        storage.read_at(0, &mut hb[..HEADER_LEN])?;
        let mut head = [0u8; HEADER_LEN];
        head.copy_from_slice(&hb[..HEADER_LEN]);
        let version = Header::version(&head)?;
        let header_len = Header::len_of(version).ok_or(RegionError::Version(version))?;
        if storage.size() < header_len as u64 {
            return Err(RegionError::Truncated);
        }
        storage.read_at(0, &mut hb[..header_len])?;
        let header = Header::parse(&hb[..header_len])?;
        if header.len != storage.size() {
            return Err(RegionError::Truncated);
        }
        let first = header_len as u64;
        let index_len = u64::from(header.tiles) * ENTRY_LEN as u64;
        if header.index < first || header.index.checked_add(index_len) != Some(header.len) {
            return Err(RegionError::Header("index"));
        }
        // Blobs lie after the header and before the string table, which
        // lies before the label index, which ends where the building index
        // starts, which ends where the index starts.
        let mut data_end = header.index;
        let mut labels_end = header.index;
        if let Some(bt) = header.buildings {
            let len = u64::from(bt.sections) * ENTRY_LEN as u64;
            if bt.index.checked_add(len) != Some(header.index) || bt.index < first {
                return Err(RegionError::Header("building index"));
            }
            labels_end = bt.index;
            data_end = bt.index;
        }
        if let Some(l) = header.labels {
            let label_index_len = u64::from(l.sections) * ENTRY_LEN as u64;
            if l.index.checked_add(label_index_len) != Some(labels_end) || l.index < first {
                return Err(RegionError::Header("label index"));
            }
            let offsets = (u64::from(l.strings) + 1) * 4;
            if l.table < first
                || l.table.checked_add(l.table_len) != Some(l.index)
                || l.table_len < offsets
            {
                return Err(RegionError::Header("strings"));
            }
            let mut last = [0u8; 4];
            storage.read_at(l.table + offsets - 4, &mut last)?;
            if u64::from(u32::from_le_bytes(last)) != l.table_len - offsets {
                return Err(RegionError::Strings(l.strings));
            }
            data_end = l.table;
        }
        let text = |(off, len): (u32, u32)| -> Result<String, RegionError> {
            if len == 0 || len > MAX_TEXT {
                return Err(RegionError::Text);
            }
            let end = u64::from(off) + u64::from(len);
            if u64::from(off) < first || end > data_end {
                return Err(RegionError::Header("text"));
            }
            let mut b = vec![0u8; len as usize];
            storage.read_at(u64::from(off), &mut b)?;
            String::from_utf8(b).map_err(|_| RegionError::Text)
        };
        let attribution = text(header.attribution)?;
        let notice = text(header.notice)?;
        Ok(Self {
            data_end,
            storage,
            header,
            attribution,
            notice,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The line every map view shows, "© OpenStreetMap contributors".
    pub fn attribution(&self) -> &str {
        &self.attribution
    }

    /// The licence and the source the region was cut from.
    pub fn notice(&self) -> &str {
        &self.notice
    }

    /// The zoom range the region holds.
    pub fn zooms(&self) -> core::ops::RangeInclusive<u8> {
        self.header.min_zoom..=self.header.max_zoom
    }

    /// Index entry `n`, checked: a valid tile in the region's zooms and a
    /// blob inside the data area.
    pub fn entry(&self, n: u32) -> Result<Entry, RegionError> {
        self.entry_in(self.header.index, self.header.tiles, n)
    }

    /// Entry `n` of the index at `index` holding `count` entries.
    fn entry_in(&self, index: u64, count: u32, n: u32) -> Result<Entry, RegionError> {
        if n >= count {
            return Err(RegionError::Entry(n));
        }
        let mut b = [0u8; ENTRY_LEN];
        self.storage
            .read_at(index + u64::from(n) * ENTRY_LEN as u64, &mut b)?;
        let e = Entry::parse(&b).ok_or(RegionError::Entry(n))?;
        let in_data = e.offset >= self.header.header_len() as u64
            && e.len > 0
            && e.offset
                .checked_add(u64::from(e.len))
                .is_some_and(|end| end <= self.data_end);
        if !e.id.is_valid() || !self.zooms().contains(&e.id.z) || !in_data {
            return Err(RegionError::Entry(n));
        }
        Ok(e)
    }

    /// The entry of `id`, by binary search over the index in place;
    /// `None` when the region has nothing there.
    pub fn find(&self, id: TileId) -> Result<Option<Entry>, RegionError> {
        self.find_in(self.header.index, self.header.tiles, id)
    }

    fn find_in(&self, index: u64, count: u32, id: TileId) -> Result<Option<Entry>, RegionError> {
        let (mut lo, mut hi) = (0u32, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = self.entry_in(index, count, mid)?;
            match e.id.cmp(&id) {
                core::cmp::Ordering::Equal => return Ok(Some(e)),
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    /// How many strings the region's table holds, 0 in a version 1 file.
    pub fn string_count(&self) -> u32 {
        self.header.labels.map_or(0, |l| l.strings)
    }

    /// Label index entry `n`, checked as [`Region::entry`] checks a tile's.
    pub fn label_entry(&self, n: u32) -> Result<Entry, RegionError> {
        let l = self.header.labels.ok_or(RegionError::Entry(n))?;
        self.entry_in(l.index, l.sections, n)
    }

    /// The label section entry of `id`; `None` when the tile has no names
    /// or the file is version 1. Only a frontend that draws names asks.
    pub fn find_labels(&self, id: TileId) -> Result<Option<Entry>, RegionError> {
        match self.header.labels {
            Some(l) => self.find_in(l.index, l.sections, id),
            None => Ok(None),
        }
    }

    /// Building index entry `n`, checked as [`Region::entry`] checks a
    /// tile's.
    pub fn building_entry(&self, n: u32) -> Result<Entry, RegionError> {
        let b = self.header.buildings.ok_or(RegionError::Entry(n))?;
        self.entry_in(b.index, b.sections, n)
    }

    /// Building sections the region holds, 0 before version 3.
    pub fn building_sections(&self) -> u32 {
        self.header.buildings.map_or(0, |b| b.sections)
    }

    /// The building section entry of `id`; `None` when the tile has no
    /// buildings or the file is older than version 3. Only a frontend that
    /// draws buildings asks.
    pub fn find_buildings(&self, id: TileId) -> Result<Option<Entry>, RegionError> {
        match self.header.buildings {
            Some(b) => self.find_in(b.index, b.sections, id),
            None => Ok(None),
        }
    }

    /// Byte range of string `n` in the storage.
    fn string_range(&self, n: u32) -> Result<(u64, u32), RegionError> {
        let l = self.header.labels.ok_or(RegionError::Strings(n))?;
        if n >= l.strings {
            return Err(RegionError::Strings(n));
        }
        let mut b = [0u8; 8];
        self.storage.read_at(l.table + u64::from(n) * 4, &mut b)?;
        let (from, to) = (u32_at(&b, 0), u32_at(&b, 4));
        let bytes = l.table_len - (u64::from(l.strings) + 1) * 4;
        if from > to || u64::from(to) > bytes {
            return Err(RegionError::Strings(n));
        }
        let start = l.table + (u64::from(l.strings) + 1) * 4 + u64::from(from);
        Ok((start, to - from))
    }

    /// String `n` of the table into `out`, replacing its contents.
    pub fn read_string(&self, n: u32, out: &mut String) -> Result<(), RegionError> {
        let (at, len) = self.string_range(n)?;
        let mut b = vec![0u8; len as usize];
        self.storage.read_at(at, &mut b)?;
        out.clear();
        out.push_str(core::str::from_utf8(&b).map_err(|_| RegionError::Strings(n))?);
        Ok(())
    }

    /// The index of `name` in the string table, by binary search: the
    /// table is sorted, so a search by place name needs no index of its own.
    pub fn find_string(&self, name: &str) -> Result<Option<u32>, RegionError> {
        let (mut lo, mut hi) = (0u32, self.string_count());
        let mut s = String::new();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            self.read_string(mid, &mut s)?;
            match s.as_bytes().cmp(name.as_bytes()) {
                core::cmp::Ordering::Equal => return Ok(Some(mid)),
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    /// Read `e`'s blob into `buf`, replacing its contents.
    pub fn read_tile(&self, e: &Entry, buf: &mut Vec<u8>) -> Result<(), RegionError> {
        buf.clear();
        buf.resize(e.len as usize, 0);
        self.storage.read_at(e.offset, buf)
    }

    /// Walk the whole index: every entry in range, strictly ascending, and
    /// every blob a one-tile fixture; from version 2 the string table and
    /// every label section too, in version 3 every building section and
    /// every record in it. What the cut runs on its own output and a
    /// frontend may run once on a file it did not make.
    pub fn verify(&self) -> Result<(), RegionError> {
        self.verify_tiles_and_labels()?;
        let mut prev: Option<TileId> = None;
        let mut sec = Vec::new();
        for n in 0..self.building_sections() {
            let e = self.building_entry(n)?;
            if prev.is_some_and(|p| p >= e.id) {
                return Err(RegionError::Entry(n));
            }
            prev = Some(e.id);
            self.read_tile(&e, &mut sec)?;
            Buildings::parse(&sec)
                .and_then(|b| b.check())
                .map_err(|err| RegionError::Buildings(e.id, err))?;
        }
        Ok(())
    }

    fn verify_tiles_and_labels(&self) -> Result<(), RegionError> {
        let mut prev: Option<TileId> = None;
        let mut buf = Vec::new();
        for n in 0..self.header.tiles {
            let e = self.entry(n)?;
            if prev.is_some_and(|p| p >= e.id) {
                return Err(RegionError::Entry(n));
            }
            prev = Some(e.id);
            self.read_tile(&e, &mut buf)?;
            check_tile(e.id, &buf)?;
        }
        let Some(l) = self.header.labels else {
            return Ok(());
        };
        // The string table: offsets ascending, every string UTF-8, sorted
        // and unique.
        let mut table = vec![0u8; l.table_len as usize];
        self.storage.read_at(l.table, &mut table)?;
        check_strings(&table, l.strings)?;
        let mut prev: Option<TileId> = None;
        let mut sec = Vec::new();
        for n in 0..l.sections {
            let e = self.label_entry(n)?;
            if prev.is_some_and(|p| p >= e.id) {
                return Err(RegionError::Entry(n));
            }
            prev = Some(e.id);
            self.read_tile(&e, &mut sec)?;
            let tile = self
                .find(e.id)?
                .ok_or(RegionError::Labels(e.id, LabelError::Length))?;
            self.read_tile(&tile, &mut buf)?;
            check_labels(e.id, &sec, l.strings, &check_tile(e.id, &buf)?)?;
        }
        Ok(())
    }
}

/// Check a string table of `count` strings whole: offsets ascending to
/// its end, each string UTF-8, the strings sorted and each once.
pub fn check_strings(table: &[u8], count: u32) -> Result<(), RegionError> {
    let offsets = (count as usize + 1) * 4;
    let bad = RegionError::Strings(count);
    if table.len() < offsets {
        return Err(bad);
    }
    let (heads, bytes) = table.split_at(offsets);
    let mut prev: Option<&[u8]> = None;
    for n in 0..count as usize {
        let (from, to) = (
            u32_at(heads, n * 4) as usize,
            u32_at(heads, n * 4 + 4) as usize,
        );
        let s = bytes.get(from..to).ok_or(RegionError::Strings(n as u32))?;
        core::str::from_utf8(s).map_err(|_| RegionError::Strings(n as u32))?;
        if prev.is_some_and(|p| p >= s) {
            return Err(RegionError::Strings(n as u32));
        }
        prev = Some(s);
    }
    if u32_at(heads, 0) != 0 || u32_at(heads, count as usize * 4) as usize != bytes.len() {
        return Err(bad);
    }
    Ok(())
}

/// Parse tile `id`'s label section against a table of `strings` strings
/// and the tile's geometry: every line label names a line feature of it.
pub fn check_labels<'a>(
    id: TileId,
    section: &'a [u8],
    strings: u32,
    tile: &Fixture<'_>,
) -> Result<Section<'a>, RegionError> {
    let s = Section::parse(section, strings).map_err(|e| RegionError::Labels(id, e))?;
    let lines: Vec<bool> = tile.features().map(|f| f.kind == Kind::Line).collect();
    for (n, l) in s.lines() {
        if !lines.get(usize::from(l.feature)).copied().unwrap_or(false) {
            return Err(RegionError::Labels(id, LabelError::Feature(n)));
        }
    }
    Ok(s)
}

/// Tile shifts a region blob may carry: extents 4096 (0) down to 256 (4).
pub const TILE_SHIFTS: core::ops::RangeInclusive<i8> = 0..=4;

/// Parse a tile blob and hold it to what the region format promises: one
/// tile at offset (0, 0), the tile view, a shift in [`TILE_SHIFTS`], and
/// every vertex within one extent of the tile. The last bound is what keeps
/// a placed vertex inside the filler's coordinate limit at any overzoom the
/// viewport selects ([`crate::viewport::MAX_OVERZOOM`]).
pub fn check_tile(id: TileId, blob: &[u8]) -> Result<Fixture<'_>, RegionError> {
    let fx = Fixture::parse(blob).map_err(|e| RegionError::Tile(id, e))?;
    let bad = RegionError::Tile(id, FixtureError::Tile(0));
    if fx.tile_count() != 1
        || fx.view() != (u32::from(TILE_VIEW), u32::from(TILE_VIEW))
        || !TILE_SHIFTS.contains(&fx.shift())
    {
        return Err(bad);
    }
    let extent = 4096i32 >> fx.shift();
    let range = -extent..2 * extent;
    for f in fx.features() {
        if f.offset.x != 0 || f.offset.y != 0 {
            return Err(bad);
        }
        for ring in f.rings() {
            for (x, y) in ring.points() {
                if !range.contains(&i32::from(x)) || !range.contains(&i32::from(y)) {
                    return Err(bad);
                }
            }
        }
    }
    Ok(fx)
}

impl<'a> Region<&'a [u8]> {
    /// A region in memory or flash, its tiles borrowed in place.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, RegionError> {
        Self::open(bytes)
    }

    /// The fixture of `id`, borrowed from the region; `None` when the
    /// region has nothing there.
    pub fn fixture(&self, id: TileId) -> Result<Option<Fixture<'a>>, RegionError> {
        let Some(e) = self.find(id)? else {
            return Ok(None);
        };
        // `entry` checked the range against the data area.
        let start = e.offset as usize;
        let blob = self
            .storage
            .get(start..start + e.len as usize)
            .ok_or(RegionError::Truncated)?;
        check_tile(id, blob).map(Some)
    }

    /// Tile `id`'s label section, borrowed from the region; `None` when it
    /// has no names. Checked against the string table, not against the
    /// tile's geometry: [`Region::verify`] does that once for a file.
    pub fn labels(&self, id: TileId) -> Result<Option<Section<'a>>, RegionError> {
        let Some(e) = self.find_labels(id)? else {
            return Ok(None);
        };
        let start = e.offset as usize;
        let blob = self
            .storage
            .get(start..start + e.len as usize)
            .ok_or(RegionError::Truncated)?;
        Section::parse(blob, self.string_count())
            .map(Some)
            .map_err(|err| RegionError::Labels(id, err))
    }

    /// Tile `id`'s building section, borrowed from the region, its head
    /// checked; `None` when it has no buildings or the file is older than
    /// version 3. The records are checked as they are read.
    pub fn buildings(&self, id: TileId) -> Result<Option<Buildings<'a>>, RegionError> {
        let Some(e) = self.find_buildings(id)? else {
            return Ok(None);
        };
        let start = e.offset as usize;
        let blob = self
            .storage
            .get(start..start + e.len as usize)
            .ok_or(RegionError::Truncated)?;
        Buildings::parse(blob)
            .map(Some)
            .map_err(|err| RegionError::Buildings(id, err))
    }

    /// String `n` of the table, borrowed from the region.
    pub fn string(&self, n: u32) -> Result<&'a str, RegionError> {
        let (at, len) = self.string_range(n)?;
        let start = at as usize;
        let b = self
            .storage
            .get(start..start + len as usize)
            .ok_or(RegionError::Truncated)?;
        core::str::from_utf8(b).map_err(|_| RegionError::Strings(n))
    }
}
