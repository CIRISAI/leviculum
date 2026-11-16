//! Points of interest and travel attributes as data (LMR5, 1685): every
//! point of interest of a region, and the travel facts the source carries
//! about a way or an area (surface, hiking difficulty, access, a
//! protected area's kind), one section per tile of the region's deepest
//! zoom, beside the geometry blob — a section of their own, as the
//! buildings are, so a board skips them for free
//! (`docs/src/concepts/loquax-expedition-map.md`, section 4).
//!
//! Kinds, names, attribute keys and values are all indices into the
//! region's one string table, so the format never needs a kind table of
//! its own: the cut keeps the source's kind (`drinking_water`, `fuel`,
//! `shelter`, `border_control`, ...) verbatim, and a reader uses the
//! kinds and attribute keys it knows and skips the rest. That is the
//! "readers skip unknown fields" rule: new kinds and new attribute keys
//! land in region files without a new format version, and flag bits a
//! reader does not know are ignored.
//!
//! All integers little-endian. Anchors are tile units at extent 4096,
//! inside the tile.
//!
//! ```text
//! poi section of one tile
//!   0  u16 pois   2 u16 ways   4 u32 attributes
//!   pois        20 bytes each:
//!     0  u32 kind string      4  u32 name string (u32::MAX: unnamed)
//!     8  u32 first attribute  12 u16 attribute count
//!    14  u8 min zoom (the source's: the shallowest view that shows it)
//!    15  u8 flags (readers ignore bits they do not know)
//!    16  i16 x   18  i16 y
//!   ways        8 bytes each: a feature of the tile's geometry blob
//!               carrying attributes (a path's surface, a protected
//!               area's kind):
//!     0  u16 feature  2 u16 attribute count  4 u32 first attribute
//!   attributes  8 bytes each: u32 key string  u32 value string
//! ```
//!
//! The attribute keys are the source's (OpenStreetMap's where it keeps
//! them): `kind_detail`, `elevation`, `opening_hours`, `access`,
//! `surface`, `tracktype`, `sac_scale`, `trail_visibility`, `ford`,
//! `ref`, `network`, and `kind` for an area whose drawn class collapsed
//! it (a military area, a nature reserve). `name:alt` is the name in the
//! locale the region was cut for, where it differs from the local one.
//! A record's attributes are one run of the attribute array; runs may
//! overlap or leave gaps, a reader never assumes otherwise.

use alloc::vec::Vec;

/// Length of a section's head.
pub const HEAD_LEN: usize = 8;
/// Length of one point record.
pub const POI_LEN: usize = 20;
/// Length of one way record.
pub const WAY_LEN: usize = 8;
/// Length of one attribute.
pub const ATTR_LEN: usize = 8;
/// The name field of an unnamed point of interest.
pub const UNNAMED: u32 = u32::MAX;
/// The extent anchors are given in, whatever the tile's geometry.
pub const POI_EXTENT: i16 = 4096;

/// Why bytes are not a poi section, or a record in it is unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoiError {
    /// The length does not match the counts.
    Length,
    /// A string index past the region's table.
    String(u32),
    /// The anchor of point `n` is outside its tile.
    Anchor(u16),
    /// A record's attribute run lies outside the attribute array.
    Attributes(u32),
    /// Way record `n` names a feature its tile does not hold.
    Feature(u16),
    /// Too many records or attributes for the counts to hold.
    Count,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A point of interest, its strings in the region's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Poi {
    /// The source's kind, verbatim: `drinking_water`, `fuel`, `shelter`.
    pub kind: u32,
    /// The local name; `None` for a bench, a crater, an unnamed spring.
    pub name: Option<u32>,
    /// The shallowest view zoom the source shows it at: the thinning
    /// rank of the display rule.
    pub min_zoom: u8,
    /// Flag bits; a reader ignores bits it does not know.
    pub flags: u8,
    /// The anchor in tile units at [`POI_EXTENT`], inside the tile.
    pub x: i16,
    pub y: i16,
    /// The run of its attributes.
    first: u32,
    count: u16,
}

/// A feature of the tile's geometry blob with travel attributes: a way's
/// state, a protected area's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WayAttrs {
    /// Index of the feature in the tile's geometry blob.
    pub feature: u16,
    first: u32,
    count: u16,
}

/// One tile's poi section, checked, read in place.
#[derive(Clone, Copy, Debug)]
pub struct Pois<'a> {
    bytes: &'a [u8],
    pois: u16,
    ways: u16,
    attrs: u32,
}

impl<'a> Pois<'a> {
    /// Check `bytes` against the counts it starts with and against a
    /// region of `strings` strings. The way records' feature indices are
    /// checked against the tile by [`crate::region::check_pois`], once
    /// per file.
    pub fn parse(bytes: &'a [u8], strings: u32) -> Result<Self, PoiError> {
        if bytes.len() < HEAD_LEN {
            return Err(PoiError::Length);
        }
        let (pois, ways, attrs) = (u16_at(bytes, 0), u16_at(bytes, 2), u32_at(bytes, 4));
        let len = HEAD_LEN
            + usize::from(pois) * POI_LEN
            + usize::from(ways) * WAY_LEN
            + attrs as usize * ATTR_LEN;
        if bytes.len() != len {
            return Err(PoiError::Length);
        }
        let s = Self {
            bytes,
            pois,
            ways,
            attrs,
        };
        let known = |i: u32| match i < strings {
            true => Ok(()),
            false => Err(PoiError::String(i)),
        };
        let run = |first: u32, count: u16| match u64::from(first) + u64::from(count)
            <= u64::from(attrs)
        {
            true => Ok(()),
            false => Err(PoiError::Attributes(first)),
        };
        for (n, p) in s.pois().enumerate() {
            known(p.kind)?;
            if let Some(name) = p.name {
                known(name)?;
            }
            run(p.first, p.count)?;
            if !(0..POI_EXTENT).contains(&p.x) || !(0..POI_EXTENT).contains(&p.y) {
                return Err(PoiError::Anchor(n as u16));
            }
        }
        for w in s.ways() {
            run(w.first, w.count)?;
        }
        for (key, value) in s.raw_attrs() {
            known(key)?;
            known(value)?;
        }
        Ok(s)
    }

    fn way_at(&self, i: usize) -> usize {
        HEAD_LEN + usize::from(self.pois) * POI_LEN + i * WAY_LEN
    }

    fn attr_at(&self, i: usize) -> usize {
        self.way_at(usize::from(self.ways)) + i * ATTR_LEN
    }

    fn raw_attrs(&self) -> impl Iterator<Item = (u32, u32)> + 'a {
        let (b, first) = (self.bytes, self.attr_at(0));
        (0..self.attrs as usize).map(move |i| {
            let at = first + i * ATTR_LEN;
            (u32_at(b, at), u32_at(b, at + 4))
        })
    }

    /// How many points of interest the section holds.
    pub fn poi_count(&self) -> u16 {
        self.pois
    }

    /// How many way records the section holds.
    pub fn way_count(&self) -> u16 {
        self.ways
    }

    /// The points of interest, in the cut's order.
    pub fn pois(&self) -> impl Iterator<Item = Poi> + 'a {
        let b = self.bytes;
        (0..usize::from(self.pois)).map(move |i| {
            let at = HEAD_LEN + i * POI_LEN;
            let name = u32_at(b, at + 4);
            Poi {
                kind: u32_at(b, at),
                name: (name != UNNAMED).then_some(name),
                first: u32_at(b, at + 8),
                count: u16_at(b, at + 12),
                min_zoom: b[at + 14],
                flags: b[at + 15],
                x: u16_at(b, at + 16) as i16,
                y: u16_at(b, at + 18) as i16,
            }
        })
    }

    /// The features of the tile's blob that carry attributes.
    pub fn ways(&self) -> impl Iterator<Item = WayAttrs> + 'a {
        let b = self.bytes;
        let first = self.way_at(0);
        (0..usize::from(self.ways)).map(move |i| {
            let at = first + i * WAY_LEN;
            WayAttrs {
                feature: u16_at(b, at),
                count: u16_at(b, at + 2),
                first: u32_at(b, at + 4),
            }
        })
    }

    /// The (key string, value string) attributes of `p`.
    pub fn attrs_of(&self, p: &Poi) -> impl Iterator<Item = (u32, u32)> + 'a {
        self.attrs_run(p.first, p.count)
    }

    /// The (key string, value string) attributes of `w`.
    pub fn way_attrs_of(&self, w: &WayAttrs) -> impl Iterator<Item = (u32, u32)> + 'a {
        self.attrs_run(w.first, w.count)
    }

    fn attrs_run(&self, first: u32, count: u16) -> impl Iterator<Item = (u32, u32)> + 'a {
        let b = self.bytes;
        let at0 = self.attr_at(first as usize);
        (0..usize::from(count)).map(move |i| {
            let at = at0 + i * ATTR_LEN;
            (u32_at(b, at), u32_at(b, at + 4))
        })
    }
}

/// A point of interest the writer encodes, attributes inline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoiRecord {
    pub kind: u32,
    pub name: Option<u32>,
    pub min_zoom: u8,
    pub flags: u8,
    pub x: i16,
    pub y: i16,
    /// (key string, value string).
    pub attrs: Vec<(u32, u32)>,
}

/// A way record the writer encodes, attributes inline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WayRecord {
    pub feature: u16,
    pub attrs: Vec<(u32, u32)>,
}

/// A poi section's bytes: the writer's half, beside the reader.
pub fn encode_section(pois: &[PoiRecord], ways: &[WayRecord]) -> Result<Vec<u8>, PoiError> {
    let attrs: usize = pois.iter().map(|p| p.attrs.len()).sum::<usize>()
        + ways.iter().map(|w| w.attrs.len()).sum::<usize>();
    let per_record = pois
        .iter()
        .map(|p| p.attrs.len())
        .chain(ways.iter().map(|w| w.attrs.len()))
        .all(|n| n <= usize::from(u16::MAX));
    if pois.len() > usize::from(u16::MAX)
        || ways.len() > usize::from(u16::MAX)
        || attrs > u32::MAX as usize
        || !per_record
    {
        return Err(PoiError::Count);
    }
    let mut b = Vec::with_capacity(
        HEAD_LEN + pois.len() * POI_LEN + ways.len() * WAY_LEN + attrs * ATTR_LEN,
    );
    b.extend_from_slice(&(pois.len() as u16).to_le_bytes());
    b.extend_from_slice(&(ways.len() as u16).to_le_bytes());
    b.extend_from_slice(&(attrs as u32).to_le_bytes());
    let mut first = 0u32;
    for p in pois {
        b.extend_from_slice(&p.kind.to_le_bytes());
        b.extend_from_slice(&p.name.unwrap_or(UNNAMED).to_le_bytes());
        b.extend_from_slice(&first.to_le_bytes());
        b.extend_from_slice(&(p.attrs.len() as u16).to_le_bytes());
        b.push(p.min_zoom);
        b.push(p.flags);
        b.extend_from_slice(&p.x.to_le_bytes());
        b.extend_from_slice(&p.y.to_le_bytes());
        first += p.attrs.len() as u32;
    }
    for w in ways {
        b.extend_from_slice(&w.feature.to_le_bytes());
        b.extend_from_slice(&(w.attrs.len() as u16).to_le_bytes());
        b.extend_from_slice(&first.to_le_bytes());
        first += w.attrs.len() as u32;
    }
    for (key, value) in pois
        .iter()
        .flat_map(|p| &p.attrs)
        .chain(ways.iter().flat_map(|w| &w.attrs))
    {
        b.extend_from_slice(&key.to_le_bytes());
        b.extend_from_slice(&value.to_le_bytes());
    }
    Ok(b)
}
