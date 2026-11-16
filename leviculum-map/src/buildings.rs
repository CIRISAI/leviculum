//! Building footprints (LMR3, 1191): the outlines of a region's buildings,
//! one section per z15 tile beside its geometry blob, bucketed into a 4 x 4
//! grid of cells so a deeper view reads only the cells it shows
//! (`docs/src/concepts/loquax-map.md`, section 4, "Buildings").
//!
//! All integers little-endian. Coordinates are tile units at extent 4096,
//! the z15 tile's own, and lie within one extent of the tile (-4096 up to
//! 8191), the bound a geometry blob keeps ([`crate::region::check_tile`]).
//!
//! ```text
//! building section of one tile
//!   head    16 cells x 16 bytes, cell c covering column c % 4 and row
//!           c / 4 of the tile, 1024 units square:
//!     0  u32 end     the end of the cell's records, from the start of the
//!                    records; a cell starts where the one before it ends
//!                    (cell 0 at 0), the last one ends at the section's end
//!     4  u32 count   footprints in the cell
//!     8  i16 x0, y0, x1, y1   the box every vertex of the cell's
//!                    footprints lies in, inclusive; all 0 in an empty cell
//!   records the footprints of cell 0, then of cell 1, and so on
//!
//! footprint   one or more rings, the outer one first, then its holes
//!   ring      varint h = n << 1 | more  (LEB128, 1 or 2 bytes; n vertices,
//!             3 to 4096 for the whole footprint; more: another ring of
//!             the same footprint follows)
//!             i16 x, i16 y   the first vertex
//!             n - 1 steps to the next vertex:
//!               i8 dx, i8 dy                 dx in -127..=127
//!               0x80, i16 dx, i16 dy         any step (the escape)
//!             the ring closes back to its first vertex
//! ```
//!
//! A footprint lies in the cell holding the centre of its box (clamped
//! into the tile), once; a reader drawing part of the tile takes every
//! cell whose box meets that part ([`Buildings::touching`]), so a
//! footprint across a cell edge is drawn from its own cell alone. A
//! building across a tile edge is in each tile's section as that tile's
//! piece, as the source cut it.
//!
//! The head is checked when a section is parsed and every record when it
//! is read, so a truncated or corrupt section is an error, never a panic;
//! skipping a cell needs nothing but its head entry, no allocation.

use alloc::vec;
use alloc::vec::Vec;

/// The tile zoom building sections are cut at; views deeper overzoom it.
pub const BUILDING_ZOOM: u8 = 15;
/// Cells a side of the grid.
pub const GRID: usize = 4;
/// Cells a section.
pub const CELLS: usize = GRID * GRID;
/// Tile units a cell side.
pub const CELL_SPAN: i32 = EXTENT / GRID as i32;
/// Length of one cell's head entry.
pub const CELL_LEN: usize = 16;
/// Length of a section's head.
pub const HEAD_LEN: usize = CELLS * CELL_LEN;
/// The tile extent coordinates are given in.
pub const EXTENT: i32 = 4096;
/// Where a coordinate may lie: within one extent of the tile.
pub const COORDS: core::ops::Range<i32> = -EXTENT..2 * EXTENT;
/// The most vertices one footprint holds, all its rings together: the
/// device's work capacity, so the M9 never skips a footprint as an overflow.
pub const MAX_VERTICES: usize = 4096;
/// The most rings one footprint holds: the device's ring capacity.
pub const MAX_RINGS: usize = 1024;
/// The step byte that says a full `i16` pair follows.
const ESCAPE: u8 = 0x80;

/// Why bytes are not a building section, or a record in it is unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildingError {
    /// The head is short, or its cell ends do not run up to the end of the
    /// section, or an empty cell has a box.
    Head,
    /// Footprint `n` of cell `cell` is truncated or out of range: its
    /// vertex or ring count, a coordinate, a vertex outside the cell's
    /// box; or the cell holds more or fewer footprints than its count.
    Record { cell: u8, n: u32 },
}

/// A box in tile units, inclusive on all sides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bbox {
    pub x0: i16,
    pub y0: i16,
    pub x1: i16,
    pub y1: i16,
}

impl Bbox {
    /// Whether the two boxes share a point.
    pub fn meets(&self, o: &Bbox) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        (i32::from(self.x0)..=i32::from(self.x1)).contains(&x)
            && (i32::from(self.y0)..=i32::from(self.y1)).contains(&y)
    }

    /// The part of a z15 tile that the tile `(x, y)` at `z` covers, `z` at
    /// least [`BUILDING_ZOOM`] and the tile inside the z15 one; at z15 the
    /// whole tile.
    pub fn of_tile(z: u8, x: u32, y: u32) -> Self {
        let deeper = u32::from(z.saturating_sub(BUILDING_ZOOM)).min(12);
        let side = EXTENT >> deeper;
        let mask = (1u32 << deeper) - 1;
        let at = |v: u32| (v & mask) as i32 * side;
        let (x0, y0) = (at(x), at(y));
        // In range of i16: the extent is 4096.
        Self {
            x0: x0 as i16,
            y0: y0 as i16,
            x1: (x0 + side) as i16,
            y1: (y0 + side) as i16,
        }
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn i16_at(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}

/// A tile's building section, its head checked.
#[derive(Clone, Copy, Debug)]
pub struct Buildings<'a> {
    head: &'a [u8],
    records: &'a [u8],
}

impl<'a> Buildings<'a> {
    /// Check the head: the cell ends ascending up to the section's end, an
    /// empty cell without a box, every box ordered. The records are
    /// checked as they are read.
    pub fn parse(b: &'a [u8]) -> Result<Self, BuildingError> {
        if b.len() < HEAD_LEN {
            return Err(BuildingError::Head);
        }
        let (head, records) = b.split_at(HEAD_LEN);
        let s = Self { head, records };
        let mut start = 0usize;
        for c in 0..CELLS {
            let at = c * CELL_LEN;
            let end = u32_at(head, at) as usize;
            let count = u32_at(head, at + 4);
            let bx = s.bbox_at(c);
            let empty = count == 0;
            let bytes = end.checked_sub(start).ok_or(BuildingError::Head)?;
            if end > records.len()
                || empty != (bytes == 0)
                || (empty && bx != Bbox::default())
                || bx.x0 > bx.x1
                || bx.y0 > bx.y1
            {
                return Err(BuildingError::Head);
            }
            start = end;
        }
        if start != records.len() {
            return Err(BuildingError::Head);
        }
        Ok(s)
    }

    fn bbox_at(&self, c: usize) -> Bbox {
        let at = c * CELL_LEN + 8;
        Bbox {
            x0: i16_at(self.head, at),
            y0: i16_at(self.head, at + 2),
            x1: i16_at(self.head, at + 4),
            y1: i16_at(self.head, at + 6),
        }
    }

    /// Cell `c` of the grid, `None` past the last.
    pub fn cell(&self, c: usize) -> Option<Cell<'a>> {
        if c >= CELLS {
            return None;
        }
        let end = |c: usize| u32_at(self.head, c * CELL_LEN) as usize;
        let start = if c == 0 { 0 } else { end(c - 1) };
        Some(Cell {
            index: c as u8,
            count: u32_at(self.head, c * CELL_LEN + 4),
            bbox: self.bbox_at(c),
            // `parse` checked the ends ascending within the records.
            bytes: self.records.get(start..end(c)).unwrap_or(&[]),
        })
    }

    /// Every cell, empty ones included, in order.
    pub fn cells(&self) -> impl Iterator<Item = Cell<'a>> + '_ {
        (0..CELLS).filter_map(|c| self.cell(c))
    }

    /// The cells with footprints whose box meets `area`: what a view of
    /// that part of the tile reads.
    pub fn touching(&self, area: Bbox) -> impl Iterator<Item = Cell<'a>> + '_ {
        self.cells()
            .filter(move |c| c.count > 0 && c.bbox.meets(&area))
    }

    /// Footprints in the whole section.
    pub fn len(&self) -> u32 {
        self.cells().map(|c| c.count).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Read every record once: what [`crate::Region::verify`] runs.
    pub fn check(&self) -> Result<(), BuildingError> {
        for c in self.cells() {
            for f in c.footprints() {
                f?;
            }
        }
        Ok(())
    }
}

/// One cell of a section.
#[derive(Clone, Copy, Debug)]
pub struct Cell<'a> {
    /// Its place in the grid, column `index % 4`, row `index / 4`.
    pub index: u8,
    pub count: u32,
    /// The box its footprints lie in.
    pub bbox: Bbox,
    bytes: &'a [u8],
}

impl<'a> Cell<'a> {
    /// The cell's footprints, each checked as it is read.
    pub fn footprints(&self) -> Footprints<'a> {
        Footprints {
            cell: self.index,
            bbox: self.bbox,
            rest: self.bytes,
            left: self.count,
            n: 0,
            failed: false,
        }
    }
}

/// The footprints of one cell, in order.
pub struct Footprints<'a> {
    cell: u8,
    bbox: Bbox,
    rest: &'a [u8],
    left: u32,
    n: u32,
    failed: bool,
}

impl<'a> Iterator for Footprints<'a> {
    type Item = Result<Footprint<'a>, BuildingError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let err = BuildingError::Record {
            cell: self.cell,
            n: self.n,
        };
        if self.left == 0 {
            if self.rest.is_empty() {
                return None;
            }
            // Bytes past the cell's last footprint.
            self.failed = true;
            return Some(Err(err));
        }
        match scan(self.rest, &self.bbox) {
            Some((f, used)) => {
                self.rest = &self.rest[used..];
                self.left -= 1;
                self.n += 1;
                Some(Ok(f))
            }
            None => {
                self.failed = true;
                Some(Err(err))
            }
        }
    }
}

/// A ring's head at `at`: vertices, whether another ring follows, and the
/// head's length; `None` when it is truncated or longer than two bytes.
fn ring_head(b: &[u8], at: usize) -> Option<(usize, bool, usize)> {
    let b0 = *b.get(at)?;
    let (h, len) = if b0 & 0x80 == 0 {
        (u32::from(b0), 1)
    } else {
        let b1 = *b.get(at + 1)?;
        if b1 & 0x80 != 0 {
            return None;
        }
        (u32::from(b0 & 0x7f) | u32::from(b1) << 7, 2)
    };
    Some(((h >> 1) as usize, h & 1 == 1, len))
}

/// The step at `at` and its length; `None` when truncated.
fn step(b: &[u8], at: usize) -> Option<((i32, i32), usize)> {
    let b0 = *b.get(at)?;
    if b0 == ESCAPE {
        let p = b.get(at + 1..at + 5)?;
        let (dx, dy) = (i16_at(p, 0), i16_at(p, 2));
        return Some(((i32::from(dx), i32::from(dy)), 5));
    }
    let b1 = *b.get(at + 1)?;
    Some(((i32::from(b0 as i8), i32::from(b1 as i8)), 2))
}

/// Check the footprint at the start of `b` against the cell's box and
/// return it with its length in bytes.
fn scan<'a>(b: &'a [u8], cell: &Bbox) -> Option<(Footprint<'a>, usize)> {
    let mut at = 0usize;
    let (mut rings, mut vertices) = (0usize, 0usize);
    let mut bbox = Bbox {
        x0: i16::MAX,
        y0: i16::MAX,
        x1: i16::MIN,
        y1: i16::MIN,
    };
    loop {
        let (n, more, len) = ring_head(b, at)?;
        at += len;
        rings += 1;
        vertices += n;
        if n < 3 || vertices > MAX_VERTICES || rings > MAX_RINGS {
            return None;
        }
        let first = b.get(at..at + 4)?;
        let (mut x, mut y) = (i32::from(i16_at(first, 0)), i32::from(i16_at(first, 2)));
        at += 4;
        for k in 0..n {
            if k > 0 {
                let ((dx, dy), len) = step(b, at)?;
                at += len;
                x += dx;
                y += dy;
            }
            if !COORDS.contains(&x) || !COORDS.contains(&y) || !cell.contains(x, y) {
                return None;
            }
            // In range of i16: COORDS is.
            bbox.x0 = bbox.x0.min(x as i16);
            bbox.y0 = bbox.y0.min(y as i16);
            bbox.x1 = bbox.x1.max(x as i16);
            bbox.y1 = bbox.y1.max(y as i16);
        }
        if !more {
            break;
        }
    }
    Some((
        Footprint {
            bytes: &b[..at],
            rings: rings as u16,
            vertices: vertices as u16,
            bbox,
        },
        at,
    ))
}

/// One building's footprint, checked whole.
#[derive(Clone, Copy, Debug)]
pub struct Footprint<'a> {
    bytes: &'a [u8],
    /// Rings, the outer one with its holes.
    pub rings: u16,
    /// Vertices of all its rings.
    pub vertices: u16,
    /// The box its vertices lie in.
    pub bbox: Bbox,
}

impl<'a> Footprint<'a> {
    /// The rings, outer first.
    pub fn rings(&self) -> Rings<'a> {
        Rings {
            bytes: self.bytes,
            at: 0,
            left: self.rings,
        }
    }
}

/// The rings of one footprint.
pub struct Rings<'a> {
    bytes: &'a [u8],
    at: usize,
    left: u16,
}

impl<'a> Iterator for Rings<'a> {
    type Item = Ring<'a>;

    fn next(&mut self) -> Option<Ring<'a>> {
        if self.left == 0 {
            return None;
        }
        // `scan` checked the bytes; a `None` here only ends the walk.
        let (n, _, len) = ring_head(self.bytes, self.at)?;
        let start = self.at + len;
        let mut at = start + 4;
        for _ in 1..n {
            at += step(self.bytes, at)?.1;
        }
        self.at = at;
        self.left -= 1;
        Some(Ring {
            bytes: self.bytes.get(start..at)?,
            len: n as u16,
        })
    }
}

/// One closed ring.
#[derive(Clone, Copy, Debug)]
pub struct Ring<'a> {
    bytes: &'a [u8],
    len: u16,
}

impl<'a> Ring<'a> {
    /// Vertices, the closing one not repeated.
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The vertices, in tile units.
    pub fn points(&self) -> Points<'a> {
        Points {
            bytes: self.bytes,
            at: 0,
            left: self.len,
            x: 0,
            y: 0,
        }
    }
}

/// The vertices of one ring.
pub struct Points<'a> {
    bytes: &'a [u8],
    at: usize,
    left: u16,
    x: i32,
    y: i32,
}

impl Iterator for Points<'_> {
    type Item = (i16, i16);

    fn next(&mut self) -> Option<(i16, i16)> {
        if self.left == 0 {
            return None;
        }
        if self.at == 0 {
            let p = self.bytes.get(..4)?;
            self.x = i32::from(i16_at(p, 0));
            self.y = i32::from(i16_at(p, 2));
            self.at = 4;
        } else {
            let ((dx, dy), len) = step(self.bytes, self.at)?;
            self.at += len;
            self.x += dx;
            self.y += dy;
        }
        self.left -= 1;
        // In range of i16: `scan` checked every vertex against COORDS.
        Some((self.x as i16, self.y as i16))
    }
}

/// The cell a footprint with box `b` lies in: the one holding its centre.
pub fn cell_of(b: &Bbox) -> usize {
    let at = |lo: i16, hi: i16| {
        let mid = (i32::from(lo) + i32::from(hi)).div_euclid(2);
        mid.div_euclid(CELL_SPAN).clamp(0, GRID as i32 - 1) as usize
    };
    at(b.y0, b.y1) * GRID + at(b.x0, b.x1)
}

/// One tile's building section from its footprints, each a list of rings
/// (outer first) in tile units at extent 4096; the writer's half, kept
/// beside the reader. Footprints keep their order within a cell. An error
/// names the first footprint the format cannot hold (`cell` is its cell,
/// `n` its place in `footprints`): no ring, a ring under three vertices,
/// more than [`MAX_VERTICES`] or [`MAX_RINGS`], or a coordinate outside
/// [`COORDS`].
pub fn encode_section(footprints: &[Vec<Vec<(i16, i16)>>]) -> Result<Vec<u8>, BuildingError> {
    let mut cells: Vec<Vec<u8>> = vec![Vec::new(); CELLS];
    let mut counts = [0u32; CELLS];
    let mut boxes: [Option<Bbox>; CELLS] = [None; CELLS];
    for (n, rings) in footprints.iter().enumerate() {
        let vertices: usize = rings.iter().map(Vec::len).sum();
        let pts = || rings.iter().flatten();
        let fits =
            |&(x, y): &(i16, i16)| COORDS.contains(&i32::from(x)) && COORDS.contains(&i32::from(y));
        let b = pts().fold(None::<Bbox>, |b, &(x, y)| {
            Some(match b {
                None => Bbox {
                    x0: x,
                    y0: y,
                    x1: x,
                    y1: y,
                },
                Some(b) => Bbox {
                    x0: b.x0.min(x),
                    y0: b.y0.min(y),
                    x1: b.x1.max(x),
                    y1: b.y1.max(y),
                },
            })
        });
        let c = b.as_ref().map_or(0, cell_of);
        let bad = BuildingError::Record {
            cell: c as u8,
            n: n as u32,
        };
        let Some(b) = b else {
            return Err(bad);
        };
        if rings.len() > MAX_RINGS
            || vertices > MAX_VERTICES
            || rings.iter().any(|r| r.len() < 3)
            || !pts().all(fits)
        {
            return Err(bad);
        }
        let out = &mut cells[c];
        for (i, ring) in rings.iter().enumerate() {
            let more = u32::from(i + 1 < rings.len());
            let h = (ring.len() as u32) << 1 | more;
            if h < 0x80 {
                out.push(h as u8);
            } else {
                out.push((h & 0x7f) as u8 | 0x80);
                out.push((h >> 7) as u8);
            }
            let (mut px, mut py) = ring[0];
            out.extend_from_slice(&px.to_le_bytes());
            out.extend_from_slice(&py.to_le_bytes());
            for &(x, y) in &ring[1..] {
                let (dx, dy) = (i32::from(x) - i32::from(px), i32::from(y) - i32::from(py));
                if (-127..=127).contains(&dx) && (-128..=127).contains(&dy) {
                    out.push(dx as i8 as u8);
                    out.push(dy as i8 as u8);
                } else {
                    // In range of i16: both ends lie in COORDS.
                    out.push(ESCAPE);
                    out.extend_from_slice(&(dx as i16).to_le_bytes());
                    out.extend_from_slice(&(dy as i16).to_le_bytes());
                }
                (px, py) = (x, y);
            }
        }
        counts[c] += 1;
        boxes[c] = Some(match boxes[c] {
            None => b,
            Some(o) => Bbox {
                x0: o.x0.min(b.x0),
                y0: o.y0.min(b.y0),
                x1: o.x1.max(b.x1),
                y1: o.y1.max(b.y1),
            },
        });
    }
    let total: usize = cells.iter().map(Vec::len).sum();
    if u32::try_from(total).is_err() {
        return Err(BuildingError::Head);
    }
    let mut out = Vec::with_capacity(HEAD_LEN + total);
    let mut end = 0u32;
    for c in 0..CELLS {
        end += cells[c].len() as u32;
        let b = boxes[c].unwrap_or_default();
        out.extend_from_slice(&end.to_le_bytes());
        out.extend_from_slice(&counts[c].to_le_bytes());
        for v in [b.x0, b.y0, b.x1, b.y1] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    for c in &cells {
        out.extend_from_slice(c);
    }
    Ok(out)
}
