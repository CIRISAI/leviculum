//! Map labels as data: the names of places, waters, parks and streets a
//! region carries (LMR2, `docs/drafts/map-renderer-roadmap-2026-10.md`,
//! section 1, "The format step: LMR2"), and the one rule every frontend
//! places them by ([`place`]).
//!
//! A region holds one string table for all its tiles and, per tile, a
//! label section beside the geometry blob. All integers little-endian.
//!
//! ```text
//! string table
//!   u32 offsets[count + 1], from the start of the bytes; offsets[0] = 0
//!   bytes   the strings, UTF-8, sorted by their bytes, each once
//!
//! label section of one tile
//!   0  u16 points   2 u16 lines   4 u16 alternates   6 u16 0
//!   points      12 bytes each: u8 kind  u8 rank  u8 min zoom  u8 flags
//!               u32 string  i16 x  i16 y (the anchor in tile units at
//!               extent 4096, inside the tile)
//!   lines       8 bytes each: u16 feature  u8 kind  u8 min zoom  u32 string
//!               (the feature is an index into the tile's geometry blob, a
//!               line feature of its own)
//!   alternates  8 bytes each, ascending by label: u16 label (points first,
//!               then lines)  u16 0  u32 string (the name in the locale the
//!               region was cut for, where it differs from the local one)
//! ```
//!
//! A reader skips a kind it does not know and ignores flag bits it does
//! not know, so a later cut can add both without breaking it; anything
//! else out of range is an error. Sorted strings make a name searchable by
//! binary search ([`crate::Region::find_string`]).

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::{Ordering, Reverse};

use crate::region::{check_tile, Region, RegionError, Storage};
use crate::viewport::Viewport;

/// Length of a section's head.
pub const SECTION_HEAD: usize = 8;
/// Length of a point label record.
pub const POINT_LEN: usize = 12;
/// Length of a line label record.
pub const LINE_LEN: usize = 8;
/// Length of an alternate name record.
pub const ALT_LEN: usize = 8;
/// The extent label anchors are given in, whatever the tile's geometry.
pub const LABEL_EXTENT: i16 = 4096;
/// Sub-pixels a pixel, the unit of [`ViewLabel`]'s positions.
pub const SUBPIXEL: i64 = leviculum_mapfill::SUBPIXEL as i64;

/// What a label names. The byte values are the format's; a reader skips a
/// byte it does not know ([`LabelKind::from_wire`] is `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LabelKind {
    City,
    Town,
    /// A district: OpenStreetMap's quarter, Protomaps' `macrohood`.
    District,
    Village,
    Suburb,
    Neighbourhood,
    /// A country's name, over the world outline where no region covers
    /// the view (1683). Declared before [`LabelKind::Water`], so it is
    /// neither a place, a line nor a road, and [`place`] serves it after
    /// the place names and before the waters.
    Country,
    /// A sea, a gulf, a bay: [`LabelKind::Water`] names a lake's area,
    /// this the ocean's.
    Sea,
    /// A mountain range's name, and a peak's
    /// (`expedition-map-concept-2026-11.md`, section 2).
    Range,
    /// A desert's name.
    Desert,
    /// A lake, a river, a sea: the name of a water area.
    Water,
    Park,
    /// A river, a canal, a stream: a waterway's name, set along its line
    /// as a street's is.
    River,
    /// A motorway or trunk road.
    MajorRoad,
    /// A primary, secondary or tertiary road.
    SecondaryRoad,
    MinorRoad,
}

impl LabelKind {
    /// Every kind, in the order [`place`] serves them.
    pub const ALL: [LabelKind; 16] = [
        LabelKind::City,
        LabelKind::Town,
        LabelKind::District,
        LabelKind::Village,
        LabelKind::Suburb,
        LabelKind::Neighbourhood,
        LabelKind::Country,
        LabelKind::Sea,
        LabelKind::Range,
        LabelKind::Desert,
        LabelKind::Water,
        LabelKind::Park,
        LabelKind::River,
        LabelKind::MajorRoad,
        LabelKind::SecondaryRoad,
        LabelKind::MinorRoad,
    ];

    /// The format's byte.
    pub const fn wire(self) -> u8 {
        match self {
            LabelKind::City => 1,
            LabelKind::Town => 2,
            LabelKind::District => 3,
            LabelKind::Village => 4,
            LabelKind::Suburb => 5,
            LabelKind::Neighbourhood => 6,
            LabelKind::Country => 7,
            LabelKind::Sea => 8,
            LabelKind::Range => 9,
            LabelKind::Desert => 10,
            LabelKind::Water => 16,
            LabelKind::Park => 17,
            LabelKind::River => 18,
            LabelKind::MajorRoad => 32,
            LabelKind::SecondaryRoad => 33,
            LabelKind::MinorRoad => 34,
        }
    }

    /// The kind of a format byte; `None` for one this reader does not know.
    pub fn from_wire(b: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.wire() == b)
    }

    /// A place: a city down to a neighbourhood.
    pub fn is_place(self) -> bool {
        self <= LabelKind::Neighbourhood
    }

    /// A street name, set along its line.
    pub fn is_road(self) -> bool {
        self >= LabelKind::MajorRoad
    }

    /// A name set along a line: a street's or a river's. It comes as a
    /// line label and repeats along its line ([`Rule::repeat`]).
    pub fn is_line(self) -> bool {
        self >= LabelKind::River
    }

    /// The order [`place`] serves labels in, lowest first: pins come before
    /// all of them, then places by class, then waters and parks, then
    /// rivers, then streets, major roads first.
    pub fn priority(self) -> u8 {
        self as u8
    }
}

/// A label at a point: a place, a water, a park.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointLabel {
    pub kind: LabelKind,
    /// Population rank for a place (Protomaps' `population_rank`), 0 where
    /// the source has none: within a kind, higher ranks are placed first.
    pub rank: u8,
    /// The shallowest view zoom that shows it.
    pub min_zoom: u8,
    pub flags: u8,
    /// Index into the region's string table: the local name.
    pub string: u32,
    /// The anchor in tile units at [`LABEL_EXTENT`], inside the tile.
    pub x: i16,
    pub y: i16,
}

/// A label along a line feature of the tile: a street's or a river's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineLabel {
    /// Index of the line feature in the tile's geometry blob.
    pub feature: u16,
    pub kind: LabelKind,
    pub min_zoom: u8,
    pub string: u32,
}

/// Why a label section or a string table is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelError {
    /// The length does not match the counts.
    Length,
    /// A reserved field is not 0.
    Reserved,
    /// A string index past the region's table.
    String(u32),
    /// The alternates are out of order or name a label that is not there.
    Alternate(u16),
    /// The anchor of point `n` is outside its tile.
    Anchor(u16),
    /// Line label `n` names a feature its tile does not hold as a line.
    Feature(u16),
    /// A string table's offsets are out of order or past its bytes, or a
    /// string is not UTF-8.
    Table,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// One tile's label section, checked, read in place.
#[derive(Clone, Copy, Debug)]
pub struct Section<'a> {
    bytes: &'a [u8],
    points: u16,
    lines: u16,
    alts: u16,
}

impl<'a> Section<'a> {
    /// Check `bytes` against the counts it starts with and against a
    /// region of `strings` strings.
    pub fn parse(bytes: &'a [u8], strings: u32) -> Result<Self, LabelError> {
        if bytes.len() < SECTION_HEAD {
            return Err(LabelError::Length);
        }
        let (points, lines, alts) = (u16_at(bytes, 0), u16_at(bytes, 2), u16_at(bytes, 4));
        if u16_at(bytes, 6) != 0 {
            return Err(LabelError::Reserved);
        }
        let len = SECTION_HEAD
            + usize::from(points) * POINT_LEN
            + usize::from(lines) * LINE_LEN
            + usize::from(alts) * ALT_LEN;
        if bytes.len() != len {
            return Err(LabelError::Length);
        }
        let s = Self {
            bytes,
            points,
            lines,
            alts,
        };
        let known = |i: u32| match i < strings {
            true => Ok(()),
            false => Err(LabelError::String(i)),
        };
        for (n, p) in s.raw_points().enumerate() {
            known(p.string)?;
            if !(0..LABEL_EXTENT).contains(&p.x) || !(0..LABEL_EXTENT).contains(&p.y) {
                return Err(LabelError::Anchor(n as u16));
            }
        }
        for l in s.raw_lines() {
            known(l.3)?;
        }
        let mut prev: Option<u16> = None;
        for (label, string) in s.alternates() {
            known(string)?;
            let labels = u32::from(points) + u32::from(lines);
            if prev.is_some_and(|p| p >= label) || u32::from(label) >= labels {
                return Err(LabelError::Alternate(label));
            }
            prev = Some(label);
        }
        for i in 0..usize::from(alts) {
            if u16_at(bytes, s.alt_at(i) + 2) != 0 {
                return Err(LabelError::Reserved);
            }
        }
        Ok(s)
    }

    fn line_at(&self, i: usize) -> usize {
        SECTION_HEAD + usize::from(self.points) * POINT_LEN + i * LINE_LEN
    }

    fn alt_at(&self, i: usize) -> usize {
        self.line_at(usize::from(self.lines)) + i * ALT_LEN
    }

    fn raw_points(&self) -> impl Iterator<Item = RawPoint> + 'a {
        let b = self.bytes;
        (0..usize::from(self.points)).map(move |i| {
            let at = SECTION_HEAD + i * POINT_LEN;
            RawPoint {
                kind: b[at],
                rank: b[at + 1],
                min_zoom: b[at + 2],
                flags: b[at + 3],
                string: u32_at(b, at + 4),
                x: u16_at(b, at + 8) as i16,
                y: u16_at(b, at + 10) as i16,
            }
        })
    }

    /// (feature, kind byte, min zoom, string) of each line label.
    fn raw_lines(&self) -> impl Iterator<Item = (u16, u8, u8, u32)> + 'a {
        let b = self.bytes;
        let first = self.line_at(0);
        (0..usize::from(self.lines)).map(move |i| {
            let at = first + i * LINE_LEN;
            (u16_at(b, at), b[at + 2], b[at + 3], u32_at(b, at + 4))
        })
    }

    /// How many point labels the section holds, known kinds or not: the
    /// label numbers of the lines start here.
    pub fn point_count(&self) -> u16 {
        self.points
    }

    /// The point labels of kinds this reader knows, each with its label
    /// number.
    pub fn points(&self) -> impl Iterator<Item = (u16, PointLabel)> + 'a {
        self.raw_points().enumerate().filter_map(|(n, p)| {
            Some((
                n as u16,
                PointLabel {
                    kind: LabelKind::from_wire(p.kind)?,
                    rank: p.rank,
                    min_zoom: p.min_zoom,
                    flags: p.flags,
                    string: p.string,
                    x: p.x,
                    y: p.y,
                },
            ))
        })
    }

    /// The line labels of kinds this reader knows, each with its label
    /// number.
    pub fn lines(&self) -> impl Iterator<Item = (u16, LineLabel)> + 'a {
        let first = self.points;
        self.raw_lines()
            .enumerate()
            .filter_map(move |(n, (feature, kind, min_zoom, string))| {
                Some((
                    first + n as u16,
                    LineLabel {
                        feature,
                        kind: LabelKind::from_wire(kind)?,
                        min_zoom,
                        string,
                    },
                ))
            })
    }

    /// Every (label number, string) alternate, ascending.
    pub fn alternates(&self) -> impl Iterator<Item = (u16, u32)> + 'a {
        let b = self.bytes;
        let first = self.alt_at(0);
        (0..usize::from(self.alts)).map(move |i| {
            let at = first + i * ALT_LEN;
            (u16_at(b, at), u32_at(b, at + 4))
        })
    }

    /// The alternate name of label `n`, by binary search.
    pub fn alternate(&self, n: u16) -> Option<u32> {
        let (mut lo, mut hi) = (0usize, usize::from(self.alts));
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = self.alt_at(mid);
            match u16_at(self.bytes, at).cmp(&n) {
                Ordering::Equal => return Some(u32_at(self.bytes, at + 4)),
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
            }
        }
        None
    }
}

struct RawPoint {
    kind: u8,
    rank: u8,
    min_zoom: u8,
    flags: u8,
    string: u32,
    x: i16,
    y: i16,
}

/// A label section's bytes: the writer's half, beside the reader. The
/// alternates are sorted here; label numbers count the points first.
pub fn encode_section(points: &[PointLabel], lines: &[LineLabel], alts: &[(u16, u32)]) -> Vec<u8> {
    let mut alts = alts.to_vec();
    alts.sort_unstable();
    let mut b = Vec::with_capacity(
        SECTION_HEAD + points.len() * POINT_LEN + lines.len() * LINE_LEN + alts.len() * ALT_LEN,
    );
    for n in [points.len(), lines.len(), alts.len(), 0] {
        b.extend_from_slice(&(n as u16).to_le_bytes());
    }
    for p in points {
        b.extend_from_slice(&[p.kind.wire(), p.rank, p.min_zoom, p.flags]);
        b.extend_from_slice(&p.string.to_le_bytes());
        b.extend_from_slice(&p.x.to_le_bytes());
        b.extend_from_slice(&p.y.to_le_bytes());
    }
    for l in lines {
        b.extend_from_slice(&l.feature.to_le_bytes());
        b.extend_from_slice(&[l.kind.wire(), l.min_zoom]);
        b.extend_from_slice(&l.string.to_le_bytes());
    }
    for (label, string) in alts {
        b.extend_from_slice(&label.to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&string.to_le_bytes());
    }
    b
}

/// A string table's bytes from strings already sorted and unique.
pub fn encode_strings<'s>(sorted: impl IntoIterator<Item = &'s str>) -> (u32, Vec<u8>) {
    let strings: Vec<&str> = sorted.into_iter().collect();
    let mut b = Vec::new();
    let mut at = 0u32;
    b.extend_from_slice(&at.to_le_bytes());
    for s in &strings {
        at += s.len() as u32;
        b.extend_from_slice(&at.to_le_bytes());
    }
    for s in &strings {
        b.extend_from_slice(s.as_bytes());
    }
    (strings.len() as u32, b)
}

/// A rectangle in the caller's unit, `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }

    /// Grown by `by` on every side.
    pub fn grown(&self, by: i32) -> Rect {
        Rect {
            x0: self.x0 - by,
            y0: self.y0 - by,
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }
}

/// A label a frontend wants placed: what it is, and the boxes its text
/// covers in the frontend's unit (one for a horizontal name, squares along
/// the baseline for a turned one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub kind: LabelKind,
    pub rank: u8,
    /// The text it shows: two candidates of one text are one name.
    pub text: String,
    /// A tie breaker stable under a pan ([`ViewLabel::at`]).
    pub at: (u32, u32),
    pub boxes: Vec<Rect>,
}

/// What [`place`] holds every candidate to, beside free boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Space every label keeps around it, in the caller's unit.
    pub margin: i32,
    /// At most one label per block of this size, by the block its first
    /// box starts in (the terminal's density rule); 0 for none.
    pub block: (i32, i32),
    /// Two labels of one street or river at least this far apart
    /// (Chebyshev, first box to first box); a place, water or park name is
    /// set once per view.
    pub repeat: i32,
}

/// The order [`place`] serves candidates in: by kind's priority, higher
/// rank first, then by string and position, so the order never depends on
/// which tile a label came from or the order tiles were read.
pub fn order(c: &[Candidate]) -> Vec<usize> {
    let mut out: Vec<usize> = (0..c.len()).collect();
    out.sort_by_key(|&i| {
        let c = &c[i];
        (c.kind.priority(), Reverse(c.rank), c.text.as_str(), c.at)
    });
    out
}

/// The one placement rule (`docs/drafts/map-renderer-roadmap-2026-10.md`,
/// section 1, "Placement"): greedy in [`order`], a label is placed when
/// its boxes, grown by the margin, cover no box of `taken` (the pins and
/// their labels, placed first and never covered) and no label placed
/// before it, and the density and repeat rules allow it; else it is
/// dropped. Returns the indices of the placed candidates in the order
/// placed. Pure, so a frame and a test agree, and run at rest only: while
/// a view moves, its labels ride along in the picture.
pub fn place(candidates: &[Candidate], taken: &[Rect], rule: Rule) -> Vec<usize> {
    let mut boxes: Vec<Rect> = taken.to_vec();
    let mut blocks: Vec<(i32, i32)> = Vec::new();
    let mut placed: Vec<usize> = Vec::new();
    for i in order(candidates) {
        let c = &candidates[i];
        let Some(first) = c.boxes.first() else {
            continue;
        };
        let same = placed
            .iter()
            .map(|&p| &candidates[p])
            .filter(|p| p.text == c.text);
        let repeated = match c.kind.is_line() {
            false => same.count() > 0,
            true => same
                .filter_map(|p| p.boxes.first())
                .any(|b| (b.x0 - first.x0).abs().max((b.y0 - first.y0).abs()) < rule.repeat),
        };
        if repeated {
            continue;
        }
        let block = (rule.block.0 > 0 && rule.block.1 > 0).then(|| {
            (
                first.x0.div_euclid(rule.block.0),
                first.y0.div_euclid(rule.block.1),
            )
        });
        if block.is_some_and(|b| blocks.contains(&b)) {
            continue;
        }
        let free = c
            .boxes
            .iter()
            .all(|b| boxes.iter().all(|t| !t.grown(rule.margin).overlaps(b)));
        if !free {
            continue;
        }
        boxes.extend(c.boxes.iter().copied());
        blocks.extend(block);
        placed.push(i);
    }
    placed
}

/// A name that lands in a view, ready for a frontend to measure and place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewLabel {
    pub kind: LabelKind,
    pub rank: u8,
    /// The shallowest view zoom that shows it: the label's own, for a
    /// line the least of its pieces'. The ladder a frame that moves holds
    /// the names it carries to (1205).
    pub min_zoom: u8,
    /// What the map shows: the name in the locale the region was cut for
    /// where the cut kept one, else the local name, never both.
    pub text: String,
    /// The local name.
    pub local: String,
    /// Stable under a pan and whatever order the tiles were read in: the
    /// anchor (or the line's first vertex) at the tile zoom's 4096 units a
    /// tile.
    pub at: (u32, u32),
    /// A point's anchor in sub-pixels from the view's top-left corner;
    /// a line's first vertex.
    pub anchor: (i64, i64),
    /// A street's or a river's line in sub-pixels, its pieces joined
    /// ([`join_pieces`]); empty for a point.
    pub line: Vec<Vec<(i64, i64)>>,
}

impl<S: Storage> Region<S> {
    /// The names of `view` this region holds and the view's zoom shows:
    /// the points whose anchor lands inside the region's area of the view,
    /// and with `lines` the streets and rivers with a vertex there, the
    /// pieces of one name over the view's tiles joined end to end first
    /// ([`join_pieces`]), one label a joined line. Appended to `out` in
    /// tile order; [`place`] orders them. A frontend that draws no line
    /// names passes `lines = false` and reads no geometry.
    pub fn view_labels(
        &self,
        view: &Viewport,
        lines: bool,
        out: &mut Vec<ViewLabel>,
    ) -> Result<(), RegionError> {
        let area = self.area(view);
        let sub = SUBPIXEL;
        let inside = |(x, y): (i64, i64)| {
            let (px, py) = (x.div_euclid(sub), y.div_euclid(sub));
            let (x0, y0) = (i64::from(area.top_left.x), i64::from(area.top_left.y));
            (x0..x0 + i64::from(area.size.width)).contains(&px)
                && (y0..y0 + i64::from(area.size.height)).contains(&py)
        };
        let (mut sec, mut blob) = (Vec::new(), Vec::new());
        // The pieces of each line name over the view's tiles, by kind and
        // the strings of its names (one table for the whole region): a
        // street is cut at its junctions and a tile's edge, so its name is
        // placed on the pieces joined.
        type Piece = ((u32, u32), Vec<(i64, i64)>);
        // Kind, local name, the name shown where it differs.
        type Name = (LabelKind, u32, Option<u32>);
        // Each with the least `min_zoom` of its pieces.
        let mut groups: Vec<(Name, u8, Vec<Piece>)> = Vec::new();
        let strings = self.string_count();
        for tile in view.tiles(self.zooms()) {
            let Some(e) = self.find_labels(tile.id)? else {
                continue;
            };
            self.read_tile(&e, &mut sec)?;
            let s =
                Section::parse(&sec, strings).map_err(|err| RegionError::Labels(tile.id, err))?;
            let origin = (i64::from(tile.origin.x), i64::from(tile.origin.y));
            let o = u32::from(tile.overzoom);
            let world = |x: i64, y: i64| {
                (
                    ((u64::from(tile.id.x) << 12) as i64 + x).clamp(0, i64::from(u32::MAX)) as u32,
                    ((u64::from(tile.id.y) << 12) as i64 + y).clamp(0, i64::from(u32::MAX)) as u32,
                )
            };
            let text = |n: u16, string: u32| -> Result<(String, String), RegionError> {
                let mut local = String::new();
                self.read_string(string, &mut local)?;
                let shown = match s.alternate(n) {
                    Some(a) => {
                        let mut alt = String::new();
                        self.read_string(a, &mut alt)?;
                        alt
                    }
                    None => local.clone(),
                };
                Ok((shown, local))
            };
            for (n, p) in s.points() {
                if p.min_zoom > view.zoom() {
                    continue;
                }
                let anchor = (
                    origin.0 + (i64::from(p.x) << o),
                    origin.1 + (i64::from(p.y) << o),
                );
                if !inside(anchor) {
                    continue;
                }
                let (text, local) = text(n, p.string)?;
                out.push(ViewLabel {
                    kind: p.kind,
                    rank: p.rank,
                    min_zoom: p.min_zoom,
                    text,
                    local,
                    at: world(i64::from(p.x), i64::from(p.y)),
                    anchor,
                    line: Vec::new(),
                });
            }
            if !lines || s.lines().all(|(_, l)| l.min_zoom > view.zoom()) {
                continue;
            }
            let Some(g) = self.find(tile.id)? else {
                continue;
            };
            self.read_tile(&g, &mut blob)?;
            let fx = check_tile(tile.id, &blob)?;
            let features: Vec<_> = fx.features().collect();
            let shift = o as i32 + i32::from(fx.shift());
            let scale = |v: i16| -> i64 {
                match shift >= 0 {
                    true => i64::from(v) << shift,
                    false => i64::from(v) >> -shift,
                }
            };
            for (n, l) in s.lines() {
                if l.min_zoom > view.zoom() {
                    continue;
                }
                let Some(f) = features.get(usize::from(l.feature)) else {
                    continue;
                };
                let to4096 = |v: i16| i64::from(v) << i32::from(fx.shift().max(0));
                let key = (l.kind, l.string, s.alternate(n));
                let at = groups.iter().position(|g| g.0 == key).unwrap_or_else(|| {
                    groups.push((key, l.min_zoom, Vec::new()));
                    groups.len() - 1
                });
                groups[at].1 = groups[at].1.min(l.min_zoom);
                for r in f.rings() {
                    let Some((x0, y0)) = r.points().next() else {
                        continue;
                    };
                    let line: Vec<(i64, i64)> = r
                        .points()
                        .map(|(x, y)| (origin.0 + scale(x), origin.1 + scale(y)))
                        .collect();
                    groups[at].2.push((world(to4096(x0), to4096(y0)), line));
                }
            }
        }
        for ((kind, string, alternate), min_zoom, pieces) in groups {
            let joined: Vec<Piece> = join_pieces(pieces)
                .into_iter()
                .filter(|(_, line)| line.iter().any(|&p| inside(p)))
                .collect();
            if joined.is_empty() {
                continue;
            }
            let mut local = String::new();
            self.read_string(string, &mut local)?;
            let text = match alternate {
                Some(a) => {
                    let mut alt = String::new();
                    self.read_string(a, &mut alt)?;
                    alt
                }
                None => local.clone(),
            };
            for (at, line) in joined {
                out.push(ViewLabel {
                    kind,
                    rank: 0,
                    min_zoom,
                    text: text.clone(),
                    local: local.clone(),
                    at,
                    anchor: line[0],
                    line: alloc::vec![line],
                });
            }
        }
        Ok(())
    }
}

/// Line pieces joined where one ends where another starts or ends, each
/// with the least of its pieces' keys (the stable tie breaker of
/// [`ViewLabel::at`]): the pieces of one street, which the source cuts at
/// every junction, become the line its name is placed along. Greedy in the
/// pieces' order, so the same pieces give the same lines; at a junction of
/// three pieces of one name, two are joined and the third stays apart. A
/// piece of fewer than two points is dropped.
pub fn join_pieces<K: Ord + Copy>(pieces: Vec<(K, Vec<(i64, i64)>)>) -> Vec<(K, Vec<(i64, i64)>)> {
    let mut left: Vec<(K, Vec<(i64, i64)>)> =
        pieces.into_iter().filter(|(_, l)| l.len() >= 2).collect();
    left.reverse();
    let mut out = Vec::new();
    while let Some((mut key, mut line)) = left.pop() {
        loop {
            let (first, last) = (line[0], line[line.len() - 1]);
            if first == last {
                break;
            }
            let Some(j) = left.iter().rposition(|(_, p)| {
                let (a, b) = (p[0], p[p.len() - 1]);
                a == last || b == last || a == first || b == first
            }) else {
                break;
            };
            let (k, mut p) = left.remove(j);
            key = key.min(k);
            let (a, b) = (p[0], p[p.len() - 1]);
            if a == last {
                line.extend_from_slice(&p[1..]);
            } else if b == last {
                p.reverse();
                line.extend_from_slice(&p[1..]);
            } else {
                if a == first {
                    p.reverse();
                }
                p.pop();
                p.extend_from_slice(&line);
                line = p;
            }
        }
        out.push((key, line));
    }
    out
}
