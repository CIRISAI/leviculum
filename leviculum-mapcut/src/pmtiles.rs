//! A PMTiles v3 archive, read by ranges from a file or (with the `http`
//! feature) a server: the header and the root directory at open, a leaf
//! directory when a tile under it is first asked for, the tiles by range.
//! A box of a planet archive costs its own tiles and the few directories
//! above them, never the whole index (1089).
//!
//! The format is the one `https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md`
//! describes: a 127-byte header, directories of varint columns (tile id
//! deltas, run lengths, lengths, offsets), leaf directories behind entries
//! of run length 0, tile ids along a Hilbert curve per zoom.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::ops::RangeInclusive;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use leviculum_map::region::{Bounds, TileId};

use crate::cut::tile_range;
use crate::Res;

const HEADER_LEN: usize = 127;
/// `internal_compression` / `tile_compression` values.
pub const NONE: u8 = 1;
pub const GZIP: u8 = 2;
/// `tile_type` MVT.
const MVT: u8 = 1;
/// Leaf directories nest; the spec's writers use one level, this allows a few.
const MAX_DEPTH: u32 = 4;

/// The header fields the cut uses.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub root: (u64, u64),
    pub metadata: (u64, u64),
    pub leaves: u64,
    pub data: u64,
    pub internal_compression: u8,
    pub tile_compression: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
}

/// The spec's promise: the header and the root directory lie in the
/// archive's first 16 KiB, so one read opens it.
const FIRST_READ: u64 = 16_384;
/// Tiles closer than this in the archive are fetched in one range: the
/// bytes between cost less than another round trip.
const GAP: u64 = 32 * 1024;
/// The largest range one request asks for.
const CHUNK_MAX: u64 = 4 * 1024 * 1024;
/// Ranges fetched at once.
const FETCHERS: usize = 4;

/// Where an archive's bytes come from: a file, or a server by HTTP range
/// requests ([`crate::http::HttpRange`]).
pub trait RangeRead: Sync {
    /// `len` bytes from `offset`; fewer only where the archive ends.
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>>;
}

impl RangeRead for File {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let end = self.metadata()?.len().min(offset.saturating_add(len));
        let mut b = vec![0u8; usize::try_from(end.saturating_sub(offset))?];
        self.read_exact_at(&mut b, offset)?;
        Ok(b)
    }
}

/// An opened archive.
pub struct Archive<R: RangeRead = File> {
    source: R,
    pub header: Header,
    root: Vec<DirEntry>,
    /// The leaf directories read so far, by their offset.
    leaves: Mutex<HashMap<u64, Arc<Vec<DirEntry>>>>,
}

/// Where one tile's blob lies: its absolute offset and length.
pub type Located = (TileId, u64, u32);

/// Tiles and their raw blobs, as one fetcher hands them back.
type Blobs = Vec<(TileId, Vec<u8>)>;

/// The raw tiles of a box, fetched: a [`crate::TileSource`] in memory.
pub struct Fetched {
    tiles: BTreeMap<TileId, Vec<u8>>,
    compression: u8,
    /// The bytes fetched, gaps between tiles included.
    pub bytes: u64,
    /// The ranges they were fetched in.
    pub requests: usize,
}

impl Fetched {
    /// Every tile fetched, ascending.
    pub fn tiles(&self) -> impl Iterator<Item = TileId> + '_ {
        self.tiles.keys().copied()
    }

    /// `t`'s blob as the archive holds it, compressed.
    pub fn raw(&self, t: TileId) -> Option<&[u8]> {
        self.tiles.get(&t).map(Vec::as_slice)
    }

    /// The decompressed MVT bytes of `t`.
    pub fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        self.tiles
            .get(&t)
            .map(|raw| decompress(raw, self.compression))
            .transpose()
    }
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

/// Parse the 127-byte header.
pub fn parse_header(b: &[u8]) -> Res<Header> {
    if b.len() < HEADER_LEN || &b[..7] != b"PMTiles" {
        return Err("not a PMTiles archive".into());
    }
    if b[7] != 3 {
        return Err(format!("PMTiles version {}, this reads 3", b[7]).into());
    }
    if b[99] != MVT {
        return Err(format!("tile type {}, this reads MVT (1)", b[99]).into());
    }
    Ok(Header {
        root: (u64_at(b, 8), u64_at(b, 16)),
        metadata: (u64_at(b, 24), u64_at(b, 32)),
        leaves: u64_at(b, 40),
        data: u64_at(b, 56),
        internal_compression: b[97],
        tile_compression: b[98],
        min_zoom: b[100],
        max_zoom: b[101],
    })
}

/// Undo the archive's compression of a directory, the metadata or a tile.
pub fn decompress(bytes: &[u8], compression: u8) -> Res<Vec<u8>> {
    match compression {
        NONE => Ok(bytes.to_vec()),
        GZIP => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
            Ok(out)
        }
        c => Err(format!("compression {c} not supported").into()),
    }
}

fn varint(b: &[u8], at: &mut usize) -> Res<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*at).ok_or("directory ends inside a varint")?;
        *at += 1;
        v |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("varint longer than 64 bits".into())
}

/// One directory entry: tile id, offset, length, run length (0: a leaf).
pub type DirEntry = (u64, u64, u32, u32);

/// Decode a directory's varint columns.
pub fn parse_directory(b: &[u8]) -> Res<Vec<DirEntry>> {
    let mut at = 0;
    let n = usize::try_from(varint(b, &mut at)?)?;
    if n > b.len() {
        return Err("directory claims more entries than it has bytes".into());
    }
    let mut e: Vec<DirEntry> = vec![(0, 0, 0, 0); n];
    let mut id = 0u64;
    for x in e.iter_mut() {
        id = id
            .checked_add(varint(b, &mut at)?)
            .ok_or("tile id overflow")?;
        x.0 = id;
    }
    for x in e.iter_mut() {
        x.3 = u32::try_from(varint(b, &mut at)?)?;
    }
    for x in e.iter_mut() {
        x.2 = u32::try_from(varint(b, &mut at)?)?;
    }
    for i in 0..n {
        let v = varint(b, &mut at)?;
        e[i].1 = if v == 0 && i > 0 {
            e[i - 1].1 + u64::from(e[i - 1].2)
        } else {
            v.checked_sub(1).ok_or("offset 0 on the first entry")?
        };
    }
    Ok(e)
}

/// The Hilbert-curve tile id of `t`.
pub fn tile_id(t: TileId) -> u64 {
    let mut acc = 0u64;
    for z in 0..t.z {
        acc += 1u64 << (2 * u32::from(z));
    }
    let n = 1u64 << t.z;
    let (mut x, mut y) = (u64::from(t.x), u64::from(t.y));
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        rotate(n, &mut x, &mut y, rx, ry);
        s /= 2;
    }
    acc + d
}

fn rotate(n: u64, x: &mut u64, y: &mut u64, rx: u64, ry: u64) {
    if ry == 0 {
        if rx == 1 {
            *x = n - 1 - *x;
            *y = n - 1 - *y;
        }
        core::mem::swap(x, y);
    }
}

/// The tile of a Hilbert-curve tile id; `None` past zoom 16.
pub fn tile_of(id: u64) -> Option<TileId> {
    let mut acc = 0u64;
    for z in 0..=leviculum_map::region::MAX_ZOOM {
        let count = 1u64 << (2 * u32::from(z));
        if id < acc + count {
            let n = 1u64 << z;
            let mut t = id - acc;
            let (mut x, mut y) = (0u64, 0u64);
            let mut s = 1u64;
            while s < n {
                let rx = 1 & (t / 2);
                let ry = 1 & (t ^ rx);
                rotate(s, &mut x, &mut y, rx, ry);
                x += s * rx;
                y += s * ry;
                t /= 4;
                s *= 2;
            }
            return Some(TileId {
                z,
                x: x as u32,
                y: y as u32,
            });
        }
        acc += count;
    }
    None
}

impl Archive<File> {
    pub fn open(path: &Path) -> Res<Self> {
        Self::from_source(File::open(path)?)
    }
}

impl<R: RangeRead> Archive<R> {
    /// Read the header and the root directory.
    pub fn from_source(source: R) -> Res<Self> {
        let first = source.read_range(0, FIRST_READ)?;
        let header = parse_header(&first)?;
        let (at, len) = header.root;
        let raw = match at.checked_add(len) {
            Some(end) if end <= first.len() as u64 => first[at as usize..end as usize].to_vec(),
            _ => source.read_range(at, len)?,
        };
        if raw.len() as u64 != len {
            return Err("the root directory is cut short".into());
        }
        let root = parse_directory(&decompress(&raw, header.internal_compression)?)?;
        Ok(Self {
            source,
            header,
            root,
            leaves: Mutex::new(HashMap::new()),
        })
    }

    fn read(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let b = self.source.read_range(offset, len)?;
        if b.len() as u64 != len {
            return Err(format!("{len} bytes at {offset}: the archive ends early").into());
        }
        Ok(b)
    }

    /// The leaf directory at `offset` (from the leaves' start), read once.
    fn leaf(&self, offset: u64, len: u32) -> Res<Arc<Vec<DirEntry>>> {
        let held = |l: &Mutex<HashMap<u64, Arc<Vec<DirEntry>>>>| {
            l.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&offset)
                .cloned()
        };
        if let Some(dir) = held(&self.leaves) {
            return Ok(dir);
        }
        let raw = self.read(self.header.leaves + offset, u64::from(len))?;
        let dir = Arc::new(parse_directory(&decompress(
            &raw,
            self.header.internal_compression,
        )?)?);
        self.leaves
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(offset, Arc::clone(&dir));
        Ok(dir)
    }

    /// Where `t`'s blob lies; `None` when the archive does not hold it.
    pub fn locate(&self, t: TileId) -> Res<Option<Located>> {
        let id = tile_id(t);
        // The root borrowed, never copied: an extract's root holds every
        // tile's entry (Germany's parts hundreds of thousands), and a copy
        // per tile made the cut of a country quadratic.
        let mut leaf: Option<Arc<Vec<DirEntry>>> = None;
        for _ in 0..=MAX_DEPTH {
            let dir: &[DirEntry] = match &leaf {
                Some(l) => l,
                None => &self.root,
            };
            // The last entry at or before `id`.
            let i = match dir.binary_search_by_key(&id, |e| e.0) {
                Ok(i) => i,
                Err(0) => return Ok(None),
                Err(i) => i - 1,
            };
            let (first, off, len, run) = dir[i];
            if run == 0 {
                leaf = Some(self.leaf(off, len)?);
                continue;
            }
            if id - first < u64::from(run) {
                return Ok(Some((t, self.header.data + off, len)));
            }
            return Ok(None);
        }
        Err("leaf directories nested too deep".into())
    }

    /// The tiles the archive holds in `bounds` at `zooms`, with where each
    /// lies, ascending by tile.
    pub fn locate_in(&self, bounds: &Bounds, zooms: RangeInclusive<u8>) -> Res<Vec<Located>> {
        let mut out = Vec::new();
        for z in zooms {
            if z < self.header.min_zoom || z > self.header.max_zoom {
                continue;
            }
            let (xs, ys) = tile_range(bounds, z);
            for xr in xs {
                for x in xr {
                    for y in ys.clone() {
                        if let Some(l) = self.locate(TileId { z, x, y })? {
                            out.push(l);
                        }
                    }
                }
            }
        }
        out.sort_unstable_by_key(|l| l.0);
        Ok(out)
    }

    /// Fetch `tiles`, near ones in one range and four ranges at a
    /// time.
    pub fn fetch(&self, tiles: &[Located]) -> Res<Fetched> {
        let mut by_offset: Vec<Located> = tiles.to_vec();
        by_offset.sort_unstable_by_key(|l| (l.1, l.0));
        // Ranges of tiles: (start, end, first index, end index).
        let mut chunks: Vec<(u64, u64, usize, usize)> = Vec::new();
        for (i, &(_, off, len)) in by_offset.iter().enumerate() {
            let end = off + u64::from(len);
            match chunks.last_mut() {
                Some(c) if off <= c.1 + GAP && end.max(c.1) - c.0 <= CHUNK_MAX => {
                    c.1 = c.1.max(end);
                    c.3 = i + 1;
                }
                _ => chunks.push((off, end, i, i + 1)),
            }
        }
        let per = chunks.len().div_ceil(FETCHERS).max(1);
        let parts: Vec<Res<Blobs>> = std::thread::scope(|s| {
            let handles: Vec<_> = chunks
                .chunks(per)
                .map(|group| {
                    let by_offset = &by_offset;
                    s.spawn(move || -> Res<Blobs> {
                        let mut got = Vec::new();
                        for &(start, end, a, b) in group {
                            let bytes = self.read(start, end - start)?;
                            for &(t, off, len) in &by_offset[a..b] {
                                let at = usize::try_from(off - start)?;
                                got.push((t, bytes[at..at + len as usize].to_vec()));
                            }
                        }
                        Ok(got)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("fetch worker panicked".into()))
                })
                .collect()
        });
        let mut fetched = Fetched {
            tiles: BTreeMap::new(),
            compression: self.header.tile_compression,
            bytes: chunks.iter().map(|c| c.1 - c.0).sum(),
            requests: chunks.len(),
        };
        for part in parts {
            fetched.tiles.extend(part?);
        }
        Ok(fetched)
    }

    /// Every tile the archive addresses, ascending: the whole index read.
    pub fn tiles(&self) -> Res<Vec<TileId>> {
        let mut out = Vec::new();
        self.walk(&self.root, 0, &mut out)?;
        out.sort_unstable();
        Ok(out)
    }

    fn walk(&self, dir: &[DirEntry], depth: u32, out: &mut Vec<TileId>) -> Res<()> {
        if depth > MAX_DEPTH {
            return Err("leaf directories nested too deep".into());
        }
        for &(id, off, length, run) in dir {
            if run == 0 {
                let leaf = self.leaf(off, length)?;
                self.walk(&leaf, depth + 1, out)?;
                continue;
            }
            for k in 0..u64::from(run) {
                out.push(tile_of(id + k).ok_or("tile id beyond zoom 16")?);
            }
        }
        Ok(())
    }

    /// The decompressed MVT bytes of `t`; `None` when the archive does not
    /// hold it.
    pub fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        let Some((_, off, len)) = self.locate(t)? else {
            return Ok(None);
        };
        let raw = self.read(off, u64::from(len))?;
        Ok(Some(decompress(&raw, self.header.tile_compression)?))
    }

    /// The metadata JSON.
    pub fn metadata(&self) -> Res<serde_json::Value> {
        let (off, len) = self.header.metadata;
        if len == 0 {
            return Ok(serde_json::Value::Null);
        }
        let raw = self.read(off, len)?;
        Ok(serde_json::from_slice(&decompress(
            &raw,
            self.header.internal_compression,
        )?)?)
    }
}

fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A synthetic archive for measurements (1527): every tile of `bounds` at
/// `zooms` holds `mvt` (uncompressed MVT) and after it a field an MVT
/// reader skips, `pad` bytes that differ from tile to tile, so no two
/// blobs are stored once and a box's bytes grow with its tiles as a real
/// archive's do. Served throttled, the size of a load shows in its time.
pub fn padded(
    mvt: &[u8],
    bounds: &Bounds,
    zooms: RangeInclusive<u8>,
    pad: usize,
    metadata: &serde_json::Value,
) -> Res<Vec<u8>> {
    let mut tiles = BTreeMap::new();
    for z in zooms {
        let (xs, ys) = tile_range(bounds, z);
        for x in xs.into_iter().flatten() {
            for y in ys.clone() {
                let t = TileId { z, x, y };
                let mut blob = mvt.to_vec();
                // Field 15, length-delimited: unknown to the Tile message.
                blob.push(15 << 3 | 2);
                put_varint(pad as u64, &mut blob);
                let mut v = tile_id(t).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                blob.extend((0..pad).map(|_| {
                    v ^= v << 13;
                    v ^= v >> 7;
                    v ^= v << 17;
                    v as u8
                }));
                tiles.insert(t, blob);
            }
        }
    }
    let mut out = Vec::new();
    write(&tiles, NONE, metadata, &mut out)?;
    Ok(out)
}

/// Write a PMTiles v3 archive of `tiles` (raw blobs as `tile_compression`
/// says, keyed by tile) with `metadata`: one uncompressed root directory,
/// no leaves, identical blobs stored once. For fixtures and tests: the
/// subset of a real archive a test serves (`leviculum-mapcut/fixtures`).
pub fn write<W: Write>(
    tiles: &BTreeMap<TileId, Vec<u8>>,
    tile_compression: u8,
    metadata: &serde_json::Value,
    mut out: W,
) -> Res<()> {
    let mut by_id: Vec<(u64, &Vec<u8>)> = tiles.iter().map(|(t, b)| (tile_id(*t), b)).collect();
    by_id.sort_unstable_by_key(|e| e.0);
    let mut data = Vec::new();
    let mut seen: HashMap<&[u8], (u64, u32)> = HashMap::new();
    let mut entries: Vec<DirEntry> = Vec::new();
    for (id, blob) in &by_id {
        let (off, len) = match seen.get(blob.as_slice()) {
            Some(&at) => at,
            None => {
                let at = (data.len() as u64, u32::try_from(blob.len())?);
                data.extend_from_slice(blob);
                seen.insert(blob.as_slice(), at);
                at
            }
        };
        entries.push((*id, off, len, 1));
    }
    let mut root = Vec::new();
    put_varint(entries.len() as u64, &mut root);
    let mut last = 0;
    for e in &entries {
        put_varint(e.0 - last, &mut root);
        last = e.0;
    }
    for e in &entries {
        put_varint(u64::from(e.3), &mut root);
    }
    for e in &entries {
        put_varint(u64::from(e.2), &mut root);
    }
    for (i, e) in entries.iter().enumerate() {
        let contiguous = i > 0 && entries[i - 1].1 + u64::from(entries[i - 1].2) == e.1;
        put_varint(if contiguous { 0 } else { e.1 + 1 }, &mut root);
    }
    let meta = serde_json::to_vec(metadata)?;
    let root_at = HEADER_LEN as u64;
    let meta_at = root_at + root.len() as u64;
    let data_at = meta_at + meta.len() as u64;
    let mut h = vec![0u8; HEADER_LEN];
    h[..7].copy_from_slice(b"PMTiles");
    h[7] = 3;
    let mut put = |at: usize, v: u64| h[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put(8, root_at);
    put(16, root.len() as u64);
    put(24, meta_at);
    put(32, meta.len() as u64);
    put(40, data_at);
    put(48, 0);
    put(56, data_at);
    put(64, data.len() as u64);
    put(72, entries.len() as u64);
    put(80, entries.len() as u64);
    put(88, seen.len() as u64);
    h[96] = 1; // clustered
    h[97] = NONE;
    h[98] = tile_compression;
    h[99] = MVT;
    h[100] = tiles.keys().map(|t| t.z).min().unwrap_or(0);
    h[101] = tiles.keys().map(|t| t.z).max().unwrap_or(0);
    out.write_all(&h)?;
    out.write_all(&root)?;
    out.write_all(&meta)?;
    out.write_all(&data)?;
    out.flush()?;
    Ok(())
}
