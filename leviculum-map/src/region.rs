//! The region file: one tile pyramid of `leviculum-mapfill`'s flat
//! geometry, read in place (`docs/src/concepts/loquax-map.md`, section 4,
//! "Region file"), from version 2 (LMR2) the names on it: one string
//! table and a label section per tile ([`crate::labels`]), and from
//! version 3 (LMR3) the buildings: a building section per z15 tile
//! ([`crate::buildings`]), and from version 4 (LMR4, 1291) the same
//! sections in an order a reader can use while the file still arrives:
//! the tile index first, then the tiles coarse to fine, the names and
//! buildings last.
//!
//! From version 5 (LMR5, 1685) the points of interest and travel
//! attributes: a poi section per tile of the deepest zoom
//! ([`crate::pois`]), with its own index after the building index.
//!
//! All integers little-endian. Offsets are from the start of the file.
//!
//! ```text
//! header  64 bytes (version 1), 96 (version 2), 112 (version 3 and 4),
//!         128 (version 5)
//!   0  "LMR1"
//!   4  u16 version (1 to 4)     6  u16 header length (64, 96 or 112)
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
//!   version 3 and 4:
//!  96  u64 building index offset  104 u32 building section count
//! 108  u32 0
//!   version 5:
//! 112  u64 poi index offset  120 u32 poi section count  124 u32 0
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
//! Version 4 keeps every field and moves the parts (1291):
//!
//! ```text
//! header  112 bytes; file length 0 while the file is being written, and
//!         the names' and buildings' fields 0 until it is complete
//! text    the attribution and the notice
//! index   one slot per tile the box lists, ascending by (z, x, y), the
//!         tile count the slots': a slot is an entry, or with length 0
//!         offset 0 "not here yet" or offset u64::MAX "nothing there"
//! tiles   in index order, so coarse to fine: z0, z1, ... the deepest last
//! labels, buildings, strings, label index, building index
//!         as in version 3, after every tile, the building index last
//! ```
//!
//! Version 5 (1685) keeps version 4's layout and adds the pois: a poi
//! section per tile of the deepest zoom that has any, written after the
//! building sections, and the poi index after the building index, last in
//! the file. A cut without pois writes version 4, which a version 4
//! reader still reads. Within a section, kinds, attribute keys and flag
//! bits a reader does not know are skipped, so new ones need no version
//! ([`crate::pois`]).
//!
//! A slot is filled after its blob is written, its offset before its
//! length, and the file length is written last, after the names' and
//! buildings' fields. So a reader of a file still arriving, by a cut
//! writing it in place or a download, knows from the slots alone which
//! tiles are complete: a slot with a length whose bytes lie inside what
//! has arrived. Everything else is [`Slot::Pending`], drawn from the
//! nearest coarser tile that is there ([`Region::stand_in`]). The names
//! and the buildings show once the whole file is there.
//!
//! A tile with nothing in it under the profile has no entry (version 4:
//! a slot saying so); a reader draws the background there. Two entries may share one blob (the cut
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
use crate::pois::{PoiError, Pois};

/// The file's first four bytes.
pub const MAGIC: [u8; 4] = *b"LMR1";
/// The format version the writer writes, the newest this reader reads:
/// the pois and travel attributes (1685).
pub const VERSION: u16 = 5;
/// The version with the index first and the tiles coarse to fine (1291)
/// and without pois, which a cut without pois writes and this reader
/// reads too.
pub const VERSION_4: u16 = 4;
/// The version with names and buildings after the tiles and the index
/// last, which this reader reads too.
pub const VERSION_3: u16 = 3;
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
/// Header length in bytes of a version 3 or 4 file.
pub const HEADER_LEN_3: usize = 112;
/// Header length in bytes of a version 5 file.
pub const HEADER_LEN_5: usize = 128;
/// A version 4 slot's offset where the tile has nothing in it.
pub const EMPTY_SLOT: u64 = u64::MAX;
/// Where the header keeps the file length, which a version 4 writer
/// writes last.
pub const LEN_AT: u64 = 56;
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
    /// A tile's poi section is not one, or a way record names a feature
    /// its tile does not hold.
    Pois(TileId, PoiError),
    /// A version 4 file is not all there: a slot still waits for its
    /// tile. What [`Region::verify`] says of a file still arriving.
    Pending(TileId),
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
    /// The pois and travel attributes: `None` before version 5 (1685).
    pub pois: Option<PoiTables>,
    /// Version 4 (1291): the index first, its slots filled as the tiles
    /// are written coarse to fine, `len` 0 until the file is complete.
    pub streamed: bool,
}

/// Where a version 5 file keeps its pois.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoiTables {
    /// The poi index: entries like the tile index's, pointing at poi
    /// sections.
    pub index: u64,
    pub sections: u32,
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
            VERSION_3 | VERSION_4 => Some(HEADER_LEN_3),
            VERSION => Some(HEADER_LEN_5),
            _ => None,
        }
    }

    /// The version this header encodes as: 5 streamed with pois, 4
    /// streamed, 3 with buildings, 2 with names alone, 1 with neither.
    pub fn version_of(&self) -> u16 {
        if self.streamed {
            return match self.pois {
                Some(_) => VERSION,
                None => VERSION_4,
            };
        }
        match (self.buildings, self.labels) {
            (Some(_), _) => VERSION_3,
            (None, Some(_)) => VERSION_2,
            (None, None) => VERSION_1,
        }
    }

    /// This header's length: 128 bytes with pois, 112 streamed or with
    /// buildings, 96 with names alone, 64 with neither.
    pub fn header_len(&self) -> usize {
        match self.version_of() {
            VERSION => HEADER_LEN_5,
            VERSION_4 | VERSION_3 => HEADER_LEN_3,
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
    /// first 64 bytes, for version 2 the 32 after them, for version 3
    /// and 4 the 16 after those, and for version 5 the 16 after those.
    /// The checks against the file's length are [`Region::open`]'s.
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
        if b[27] != 0
            || (version >= VERSION_3 && u32_at(b, 108) != 0)
            || (version >= VERSION && u32_at(b, 124) != 0)
        {
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
            buildings: (version >= VERSION_3).then(|| BuildingTables {
                index: u64_at(b, 96),
                sections: u32_at(b, 104),
            }),
            pois: (version >= VERSION).then(|| PoiTables {
                index: u64_at(b, 112),
                sections: u32_at(b, 120),
            }),
            streamed: version >= VERSION_4,
        })
    }

    /// The header bytes: version 4 streamed, 3 with buildings, 2 with
    /// names alone, 1 with neither ([`Header::version_of`]); the writer's
    /// half, kept
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
        if version >= VERSION {
            let p = self.pois.unwrap_or_default();
            b[112..120].copy_from_slice(&p.index.to_le_bytes());
            b[120..124].copy_from_slice(&p.sections.to_le_bytes());
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

/// What a tile's index entry says: its blob, nothing there, or (in a
/// version 4 file still arriving) not here yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Here(Entry),
    /// The region holds nothing there: draw the background.
    Empty,
    /// The tile has not arrived: draw a stand-in ([`Region::stand_in`]).
    Pending,
}

/// An opened region: the header checked against the storage, the two
/// texts read, the index left where it is.
pub struct Region<S> {
    storage: S,
    header: Header,
    attribution: String,
    notice: String,
    /// Where the blobs may lie: after the header (version 4: after the
    /// index) ...
    data_start: u64,
    /// ... and before the index (version 4: before the names, or the
    /// bytes there when the file is not complete).
    data_end: u64,
    /// Whether every byte the header promises is there; only a version 4
    /// file may be opened without.
    complete: bool,
}

impl<S: Storage> Region<S> {
    /// Check the header against the storage and read the two texts.
    pub fn open(storage: S) -> Result<Self, RegionError> {
        let mut hb = [0u8; HEADER_LEN_5];
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
        let size = storage.size();
        let first = header_len as u64;
        let index_len = u64::from(header.tiles) * ENTRY_LEN as u64;
        let index_end = header
            .index
            .checked_add(index_len)
            .ok_or(RegionError::Header("index"))?;
        if header.index < first {
            return Err(RegionError::Header("index"));
        }
        let (header, complete) = if header.streamed {
            // Length 0 while the writer writes, longer than the storage
            // while a copy arrives; the slots must all be there.
            if (header.len != 0 && header.len < size) || index_end > size {
                return Err(RegionError::Truncated);
            }
            if header.len == size {
                (header, true)
            } else {
                // The names, the buildings and the pois come last and
                // count once the file is complete.
                let header = Header {
                    labels: None,
                    buildings: None,
                    pois: None,
                    ..header
                };
                (header, false)
            }
        } else {
            if header.len != size {
                return Err(RegionError::Truncated);
            }
            if index_end != header.len {
                return Err(RegionError::Header("index"));
            }
            (header, true)
        };
        // Blobs lie after the header (version 4: after the index) and
        // before the string table, which lies before the label index,
        // which ends where the building index starts, which ends where
        // the poi index starts (version 5) or the index (version 3), the
        // last of which ends the file (version 4 and 5: the file's end).
        let (data_start, tail) = match header.streamed {
            true => (index_end, header.len),
            false => (first, header.index),
        };
        let mut data_end = match complete {
            true => tail,
            false => size,
        };
        let mut buildings_end = tail;
        if let Some(p) = header.pois {
            let len = u64::from(p.sections) * ENTRY_LEN as u64;
            if p.index.checked_add(len) != Some(tail) || p.index < data_start {
                return Err(RegionError::Header("poi index"));
            }
            buildings_end = p.index;
            data_end = p.index;
        }
        let mut labels_end = buildings_end;
        if let Some(bt) = header.buildings {
            let len = u64::from(bt.sections) * ENTRY_LEN as u64;
            if bt.index.checked_add(len) != Some(buildings_end) || bt.index < data_start {
                return Err(RegionError::Header("building index"));
            }
            labels_end = bt.index;
            data_end = bt.index;
        }
        if let Some(l) = header.labels {
            let label_index_len = u64::from(l.sections) * ENTRY_LEN as u64;
            if l.index.checked_add(label_index_len) != Some(labels_end) || l.index < data_start {
                return Err(RegionError::Header("label index"));
            }
            let offsets = (u64::from(l.strings) + 1) * 4;
            if l.table < data_start
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
            let before = match header.streamed {
                true => header.index,
                false => data_end,
            };
            if u64::from(off) < first || end > before {
                return Err(RegionError::Header("text"));
            }
            let mut b = vec![0u8; len as usize];
            storage.read_at(u64::from(off), &mut b)?;
            String::from_utf8(b).map_err(|_| RegionError::Text)
        };
        let attribution = text(header.attribution)?;
        let notice = text(header.notice)?;
        Ok(Self {
            data_start,
            data_end,
            complete,
            storage,
            header,
            attribution,
            notice,
        })
    }

    /// Whether the whole file is there: always but for a version 4 file
    /// still being written or downloaded, whose tiles that arrived draw
    /// and whose names and buildings wait for the rest.
    pub fn is_complete(&self) -> bool {
        self.complete
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
    /// blob inside the data area. A version 4 slot that holds no blob is
    /// [`RegionError::Pending`] or, for nothing there, an
    /// [`RegionError::Entry`]; [`Region::slot`] tells them apart.
    pub fn entry(&self, n: u32) -> Result<Entry, RegionError> {
        self.entry_in(self.header.index, self.header.tiles, n)
    }

    /// Index slot `n`: its tile and what it holds.
    pub fn slot(&self, n: u32) -> Result<(TileId, Slot), RegionError> {
        let e = self.raw_entry(self.header.index, self.header.tiles, n)?;
        Ok((e.id, self.slot_of(e, n)?))
    }

    /// Entry `n` of the index at `index` holding `count` entries, holding
    /// a blob.
    fn entry_in(&self, index: u64, count: u32, n: u32) -> Result<Entry, RegionError> {
        let e = self.raw_entry(index, count, n)?;
        match self.slot_of(e, n)? {
            Slot::Here(e) => Ok(e),
            Slot::Pending => Err(RegionError::Pending(e.id)),
            Slot::Empty => Err(RegionError::Entry(n)),
        }
    }

    /// Entry `n` as it is written, its tile checked: valid and in the
    /// region's zooms.
    fn raw_entry(&self, index: u64, count: u32, n: u32) -> Result<Entry, RegionError> {
        if n >= count {
            return Err(RegionError::Entry(n));
        }
        let mut b = [0u8; ENTRY_LEN];
        self.storage
            .read_at(index + u64::from(n) * ENTRY_LEN as u64, &mut b)?;
        let e = Entry::parse(&b).ok_or(RegionError::Entry(n))?;
        if !e.id.is_valid() || !self.zooms().contains(&e.id.z) {
            return Err(RegionError::Entry(n));
        }
        Ok(e)
    }

    /// What entry `e` (number `n`) holds: a blob inside the data area, or
    /// in version 4 nothing there, or not yet.
    fn slot_of(&self, e: Entry, n: u32) -> Result<Slot, RegionError> {
        if self.header.streamed && e.len == 0 {
            // The writer writes a slot's offset before its length, so a
            // slot without a length is waiting, whatever its offset says
            // so far, unless the whole file is there.
            return match e.offset {
                EMPTY_SLOT => Ok(Slot::Empty),
                0 => Ok(Slot::Pending),
                _ if !self.complete => Ok(Slot::Pending),
                _ => Err(RegionError::Entry(n)),
            };
        }
        if e.len == 0 || e.offset < self.data_start {
            return Err(RegionError::Entry(n));
        }
        match e.offset.checked_add(u64::from(e.len)) {
            Some(end) if end <= self.data_end => Ok(Slot::Here(e)),
            // A blob past the bytes there arrives later.
            Some(_) if !self.complete => Ok(Slot::Pending),
            _ => Err(RegionError::Entry(n)),
        }
    }

    /// The entry of `id`, by binary search over the index in place;
    /// `None` when the region has nothing there, or (version 4) not yet.
    pub fn find(&self, id: TileId) -> Result<Option<Entry>, RegionError> {
        match self.find_slot(id)? {
            Slot::Here(e) => Ok(Some(e)),
            Slot::Empty | Slot::Pending => Ok(None),
        }
    }

    /// What the region holds of `id`: [`Slot::Empty`] where the index has
    /// no entry.
    pub fn find_slot(&self, id: TileId) -> Result<Slot, RegionError> {
        match self.search(self.header.index, self.header.tiles, id)? {
            Some(n) => Ok(self.slot(n)?.1),
            None => Ok(Slot::Empty),
        }
    }

    /// The nearest coarser tile that stands in for `id` while `id` has
    /// not arrived, and how many zooms coarser it is; `None` where no
    /// coarser one has arrived either, or one says nothing is there.
    pub fn stand_in(&self, id: TileId) -> Result<Option<(Entry, u8)>, RegionError> {
        let lo = self.header.min_zoom;
        for dz in 1..=id.z.saturating_sub(lo) {
            let up = TileId {
                z: id.z - dz,
                x: id.x >> dz,
                y: id.y >> dz,
            };
            match self.find_slot(up)? {
                Slot::Here(e) => return Ok(Some((e, dz))),
                Slot::Empty => return Ok(None),
                Slot::Pending => {}
            }
        }
        Ok(None)
    }

    /// Whether a tile of the box `b` at zoom `z` has arrived (1527): what
    /// the first-tile measurement reads, the next frame over `b` draws it.
    /// The box must not cross the date line.
    pub fn arrived_in(&self, b: &Bounds, z: u8) -> Result<bool, RegionError> {
        use crate::mercator::{x_of_lon, y_of_lat};
        if !self.zooms().contains(&z) {
            return Ok(false);
        }
        let s = 32 - u32::from(z);
        let col = |lon| (u64::from(x_of_lon(lon)) >> s) as u32;
        let row = |lat| (u64::from(y_of_lat(lat)) >> s) as u32;
        for x in col(b.west)..=col(b.east) {
            for y in row(b.north)..=row(b.south) {
                if let Slot::Here(_) = self.find_slot(TileId { z, x, y })? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// How many of the index's slots hold their tile, and how many there
    /// are: what a frontend shows of a file still arriving. A whole file
    /// counts every slot; it reads every slot, so it is for a log or a
    /// test, not for every frame.
    pub fn arrived(&self) -> Result<(u32, u32), RegionError> {
        let mut here = 0;
        for n in 0..self.header.tiles {
            if !matches!(self.slot(n)?.1, Slot::Pending) {
                here += 1;
            }
        }
        Ok((here, self.header.tiles))
    }

    fn find_in(&self, index: u64, count: u32, id: TileId) -> Result<Option<Entry>, RegionError> {
        match self.search(index, count, id)? {
            Some(n) => self.entry_in(index, count, n).map(Some),
            None => Ok(None),
        }
    }

    /// The number of `id`'s entry in the index at `index`, by binary
    /// search over the entries' tiles alone.
    fn search(&self, index: u64, count: u32, id: TileId) -> Result<Option<u32>, RegionError> {
        let (mut lo, mut hi) = (0u32, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = self.raw_entry(index, count, mid)?;
            match e.id.cmp(&id) {
                core::cmp::Ordering::Equal => return Ok(Some(mid)),
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

    /// Poi index entry `n`, checked as [`Region::entry`] checks a tile's.
    pub fn poi_entry(&self, n: u32) -> Result<Entry, RegionError> {
        let p = self.header.pois.ok_or(RegionError::Entry(n))?;
        self.entry_in(p.index, p.sections, n)
    }

    /// Poi sections the region holds, 0 before version 5.
    pub fn poi_sections(&self) -> u32 {
        self.header.pois.map_or(0, |p| p.sections)
    }

    /// The poi section entry of `id`; `None` when the tile has no pois or
    /// the file is older than version 5. Only a frontend that shows pois
    /// asks, so they cost every other reader nothing.
    pub fn find_pois(&self, id: TileId) -> Result<Option<Entry>, RegionError> {
        match self.header.pois {
            Some(p) => self.find_in(p.index, p.sections, id),
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
    /// every label section too, from version 3 every building section and
    /// every record in it, from version 5 every poi section against the
    /// string table and its tile's features. What the cut runs on its own
    /// output and a frontend may run once on a file it did not make. A
    /// version 4 file still arriving is [`RegionError::Pending`] at its
    /// first tile not there.
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
        let mut prev: Option<TileId> = None;
        let mut buf = Vec::new();
        for n in 0..self.poi_sections() {
            let e = self.poi_entry(n)?;
            if prev.is_some_and(|p| p >= e.id) {
                return Err(RegionError::Entry(n));
            }
            prev = Some(e.id);
            self.read_tile(&e, &mut sec)?;
            // A tile with pois but nothing drawn has no blob: its way
            // records then have no features to name.
            let features = match self.find(e.id)? {
                Some(tile) => {
                    self.read_tile(&tile, &mut buf)?;
                    check_tile(e.id, &buf)?.features().count()
                }
                None => 0,
            };
            check_pois(e.id, &sec, self.string_count(), features)?;
        }
        Ok(())
    }

    fn verify_tiles_and_labels(&self) -> Result<(), RegionError> {
        let mut prev: Option<TileId> = None;
        let mut buf = Vec::new();
        for n in 0..self.header.tiles {
            let (id, slot) = self.slot(n)?;
            if prev.is_some_and(|p| p >= id) {
                return Err(RegionError::Entry(n));
            }
            prev = Some(id);
            match slot {
                Slot::Here(e) => {
                    self.read_tile(&e, &mut buf)?;
                    check_tile(e.id, &buf)?;
                }
                Slot::Empty => {}
                Slot::Pending => return Err(RegionError::Pending(id)),
            }
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

/// Parse tile `id`'s poi section against a table of `strings` strings and
/// the count of features its tile's blob holds: every way record names
/// one of them.
pub fn check_pois<'a>(
    id: TileId,
    section: &'a [u8],
    strings: u32,
    features: usize,
) -> Result<Pois<'a>, RegionError> {
    let s = Pois::parse(section, strings).map_err(|e| RegionError::Pois(id, e))?;
    for w in s.ways() {
        if usize::from(w.feature) >= features {
            return Err(RegionError::Pois(id, PoiError::Feature(w.feature)));
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
    /// region has nothing there, or (version 4) not yet.
    pub fn fixture(&self, id: TileId) -> Result<Option<Fixture<'a>>, RegionError> {
        let Some(e) = self.find(id)? else {
            return Ok(None);
        };
        self.fixture_of(&e)
    }

    /// The fixture of entry `e`, borrowed from the region.
    pub fn fixture_of(&self, e: &Entry) -> Result<Option<Fixture<'a>>, RegionError> {
        // `entry` checked the range against the data area.
        let start = e.offset as usize;
        let blob = self
            .storage
            .get(start..start + e.len as usize)
            .ok_or(RegionError::Truncated)?;
        check_tile(e.id, blob).map(Some)
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

    /// Tile `id`'s poi section, borrowed from the region, checked against
    /// the string table; `None` when it has no pois or the file is older
    /// than version 5. The way records' feature indices are checked by
    /// [`Region::verify`], once for a file.
    pub fn pois(&self, id: TileId) -> Result<Option<Pois<'a>>, RegionError> {
        let Some(e) = self.find_pois(id)? else {
            return Ok(None);
        };
        let start = e.offset as usize;
        let blob = self
            .storage
            .get(start..start + e.len as usize)
            .ok_or(RegionError::Truncated)?;
        Pois::parse(blob, self.string_count())
            .map(Some)
            .map_err(|err| RegionError::Pois(id, err))
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
