//! The GeoTIFF subset Copernicus GLO-30 ships (order 1740): little-endian
//! classic TIFF, one band of 32-bit floats in tiles, DEFLATE with the
//! floating-point predictor (3) or uncompressed, georeferenced by
//! ModelPixelScale and one ModelTiepoint on WGS 84 degrees. Only the first
//! image (full resolution) is read; the overviews after it are skipped.
//! Anything else is refused by name, never guessed at.

use std::io::Read;

use leviculum_mapcut::Res;

const TAG_WIDTH: u16 = 256;
const TAG_HEIGHT: u16 = 257;
const TAG_BITS: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_SAMPLES: u16 = 277;
const TAG_PLANAR: u16 = 284;
const TAG_PREDICTOR: u16 = 317;
const TAG_TILE_WIDTH: u16 = 322;
const TAG_TILE_HEIGHT: u16 = 323;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_COUNTS: u16 = 325;
const TAG_SAMPLE_FORMAT: u16 = 339;
const TAG_PIXEL_SCALE: u16 = 33550;
const TAG_TIEPOINT: u16 = 33922;
const TAG_GEO_KEYS: u16 = 34735;
const TAG_GDAL_NODATA: u16 = 42113;
/// GeoKey GTRasterTypeGeoKey: 1 pixel is area, 2 pixel is point.
const KEY_RASTER_TYPE: u16 = 1025;

/// The largest cell edge this reader accepts, in pixels: a GLO-30 cell is
/// at most 3600 x 3600 (1" spacing over 1°; the columns thin out towards
/// the poles, the rows never exceed 3600). Anything larger is not a GLO-30
/// cell, and `width * height` of a hostile header could wrap.
pub const MAX_CELL_EDGE: u64 = 3600;
/// The largest internal tile edge accepted: GLO-30's COGs are tiled
/// 1024 x 1024.
pub const MAX_TILE_EDGE: u64 = 1024;

/// One decoded source cell: a grid of heights and where its pixels sit.
#[derive(Debug)]
pub struct Raster {
    /// Columns.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// Row-major heights in metres, north row first; no-data and
    /// non-finite values already replaced by 0 (sea level) and counted in
    /// [`Raster::nodata`].
    pub data: Vec<f32>,
    /// Longitude of pixel (0, 0)'s centre, degrees.
    pub lon0: f64,
    /// Latitude of pixel (0, 0)'s centre, degrees.
    pub lat0: f64,
    /// Degrees per column.
    pub dx: f64,
    /// Degrees per row (positive; rows run south).
    pub dy: f64,
    /// How many pixels were no-data or not finite (written as 0).
    pub nodata: u64,
}

/// Where a cell's pixels sit, from its header alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Georef {
    /// Columns.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// Longitude of pixel (0, 0)'s centre, degrees.
    pub lon0: f64,
    /// Latitude of pixel (0, 0)'s centre, degrees.
    pub lat0: f64,
    /// Degrees per column.
    pub dx: f64,
    /// Degrees per row (positive; rows run south).
    pub dy: f64,
}

fn err(what: &str) -> Box<dyn std::error::Error + Send + Sync> {
    format!("GeoTIFF: {what}").into()
}

/// One IFD entry: tag, type, count and where its value or value offset
/// sits in the file.
struct Entry {
    tag: u16,
    typ: u16,
    count: usize,
    at: usize,
}

struct Ifd<'a> {
    file: &'a [u8],
    entries: Vec<Entry>,
}

fn type_size(t: u16) -> Option<usize> {
    match t {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

fn u16_at(b: &[u8], at: usize) -> Res<u16> {
    let s = b.get(at..at + 2).ok_or_else(|| err("truncated"))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Res<u32> {
    let s = b.get(at..at + 4).ok_or_else(|| err("truncated"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl<'a> Ifd<'a> {
    fn read(file: &'a [u8]) -> Res<Self> {
        if file.get(..4) != Some(b"II*\0") {
            return Err(err("not a little-endian classic TIFF"));
        }
        let at = u32_at(file, 4)? as usize;
        let count = usize::from(u16_at(file, at)?);
        let mut entries = Vec::with_capacity(count);
        for n in 0..count {
            let e = at + 2 + 12 * n;
            entries.push(Entry {
                tag: u16_at(file, e)?,
                typ: u16_at(file, e + 2)?,
                count: u32_at(file, e + 4)? as usize,
                at: e + 8,
            });
        }
        Ok(Ifd { file, entries })
    }

    /// The value bytes of `tag`: inline in the entry or at its offset.
    fn raw(&self, tag: u16) -> Res<Option<(u16, usize, &'a [u8])>> {
        let Some(e) = self.entries.iter().find(|e| e.tag == tag) else {
            return Ok(None);
        };
        let size = type_size(e.typ).ok_or_else(|| err("unknown field type"))?;
        let total = size.checked_mul(e.count).ok_or_else(|| err("field size"))?;
        let start = if total <= 4 {
            e.at
        } else {
            u32_at(self.file, e.at)? as usize
        };
        let end = start.checked_add(total).ok_or_else(|| err("field end"))?;
        let bytes = self
            .file
            .get(start..end)
            .ok_or_else(|| err("field outside the file"))?;
        Ok(Some((e.typ, e.count, bytes)))
    }

    fn uints(&self, tag: u16) -> Res<Option<Vec<u64>>> {
        let Some((typ, count, b)) = self.raw(tag)? else {
            return Ok(None);
        };
        let v = match typ {
            3 => (0..count)
                .map(|i| u64::from(u16::from_le_bytes([b[2 * i], b[2 * i + 1]])))
                .collect(),
            4 => (0..count)
                .map(|i| {
                    u64::from(u32::from_le_bytes([
                        b[4 * i],
                        b[4 * i + 1],
                        b[4 * i + 2],
                        b[4 * i + 3],
                    ]))
                })
                .collect(),
            _ => return Err(err("integer field of another type")),
        };
        Ok(Some(v))
    }

    fn uint(&self, tag: u16) -> Res<Option<u64>> {
        Ok(self.uints(tag)?.and_then(|v| v.first().copied()))
    }

    fn need(&self, tag: u16, what: &str) -> Res<u64> {
        self.uint(tag)?
            .ok_or_else(|| err(&format!("no {what} tag")))
    }

    fn doubles(&self, tag: u16) -> Res<Option<Vec<f64>>> {
        let Some((typ, count, b)) = self.raw(tag)? else {
            return Ok(None);
        };
        if typ != 12 {
            return Err(err("double field of another type"));
        }
        Ok(Some(
            (0..count)
                .map(|i| {
                    let mut v = [0u8; 8];
                    v.copy_from_slice(&b[8 * i..8 * i + 8]);
                    f64::from_le_bytes(v)
                })
                .collect(),
        ))
    }

    fn ascii(&self, tag: u16) -> Res<Option<String>> {
        let Some((typ, _, b)) = self.raw(tag)? else {
            return Ok(None);
        };
        if typ != 2 {
            return Err(err("text field of another type"));
        }
        Ok(Some(
            String::from_utf8_lossy(b)
                .trim_end_matches('\0')
                .trim()
                .to_owned(),
        ))
    }
}

/// Undo the floating-point predictor on one decompressed tile row: the
/// bytes were differenced left to right, then stored as byte planes, most
/// significant plane first.
fn unpredict_row(row: &mut [u8], out: &mut [f32]) {
    for i in 1..row.len() {
        row[i] = row[i].wrapping_add(row[i - 1]);
    }
    let n = out.len();
    for (i, v) in out.iter_mut().enumerate() {
        *v = f32::from_be_bytes([row[i], row[n + i], row[2 * n + i], row[3 * n + i]]);
    }
}

/// Checked image and tile dimensions against [`MAX_CELL_EDGE`] and
/// [`MAX_TILE_EDGE`]: (width, height, tile width, tile height).
fn dimensions(ifd: &Ifd) -> Res<(usize, usize, usize, usize)> {
    let width = ifd.need(TAG_WIDTH, "width")?;
    let height = ifd.need(TAG_HEIGHT, "height")?;
    let tw = ifd.need(TAG_TILE_WIDTH, "tile width")?;
    let th = ifd.need(TAG_TILE_HEIGHT, "tile height")?;
    if width > MAX_CELL_EDGE || height > MAX_CELL_EDGE {
        return Err(err(&format!(
            "cell claims {width} x {height} pixels, over the GLO-30 bound of {MAX_CELL_EDGE} a side"
        )));
    }
    if tw > MAX_TILE_EDGE || th > MAX_TILE_EDGE {
        return Err(err(&format!(
            "cell claims {tw} x {th} pixel tiles, over the GLO-30 bound of {MAX_TILE_EDGE} a side"
        )));
    }
    if tw == 0 || th == 0 || width == 0 || height == 0 {
        return Err(err("empty image"));
    }
    // Inside the bounds above both products fit any usize; checked anyway
    // so the bound and the arithmetic cannot drift apart.
    let pixels = width.checked_mul(height);
    let tile_bytes = tw.checked_mul(th).and_then(|n| n.checked_mul(4));
    if pixels.is_none() || tile_bytes.is_none() {
        return Err(err("cell dimensions overflow"));
    }
    Ok((width as usize, height as usize, tw as usize, th as usize))
}

/// Where the pixels of the cell whose header `file` starts with sit: the
/// first IFD's dimensions, pixel scale, tiepoint and raster type, without
/// a tile. A file cut short after its IFD values reads.
pub fn georef(file: &[u8]) -> Res<Georef> {
    let ifd = Ifd::read(file)?;
    georef_of(&ifd)
}

fn georef_of(ifd: &Ifd) -> Res<Georef> {
    let (width, height, _, _) = dimensions(ifd)?;
    let scale = ifd
        .doubles(TAG_PIXEL_SCALE)?
        .ok_or_else(|| err("no pixel scale"))?;
    let tie = ifd
        .doubles(TAG_TIEPOINT)?
        .ok_or_else(|| err("no tiepoint"))?;
    if scale.len() < 2 || tie.len() < 6 || scale[0] <= 0.0 || scale[1] <= 0.0 {
        return Err(err("pixel scale or tiepoint"));
    }
    // Pixel is point unless the geokeys say area: GLO-30 names the centre
    // of pixel (0, 0) in its tiepoint.
    let keys = ifd.uints(TAG_GEO_KEYS)?.unwrap_or_default();
    let mut point = false;
    for k in keys.chunks(4).skip(1) {
        if k.len() == 4 && k[0] == u64::from(KEY_RASTER_TYPE) && k[1] == 0 {
            point = k[3] == 2;
        }
    }
    let (dx, dy) = (scale[0], scale[1]);
    let shift = if point { 0.0 } else { 0.5 };
    Ok(Georef {
        width,
        height,
        lon0: tie[3] + (shift - tie[0]) * dx,
        lat0: tie[4] - (shift - tie[1]) * dy,
        dx,
        dy,
    })
}

/// Decode a GLO-30 style GeoTIFF into a [`Raster`].
pub fn decode(file: &[u8]) -> Res<Raster> {
    let ifd = Ifd::read(file)?;
    let (width, height, tw, th) = dimensions(&ifd)?;
    if ifd.uint(TAG_BITS)?.unwrap_or(1) != 32 || ifd.uint(TAG_SAMPLE_FORMAT)?.unwrap_or(1) != 3 {
        return Err(err("not 32-bit floating point samples"));
    }
    if ifd.uint(TAG_SAMPLES)?.unwrap_or(1) != 1 || ifd.uint(TAG_PLANAR)?.unwrap_or(1) != 1 {
        return Err(err("not a single band"));
    }
    let compression = ifd.uint(TAG_COMPRESSION)?.unwrap_or(1);
    if !matches!(compression, 1 | 8 | 32946) {
        return Err(err(&format!("compression {compression}")));
    }
    let predictor = ifd.uint(TAG_PREDICTOR)?.unwrap_or(1);
    if !matches!(predictor, 1 | 3) {
        return Err(err(&format!("predictor {predictor}")));
    }
    let offsets = ifd
        .uints(TAG_TILE_OFFSETS)?
        .ok_or_else(|| err("not tiled"))?;
    let counts = ifd
        .uints(TAG_TILE_COUNTS)?
        .ok_or_else(|| err("no tile byte counts"))?;
    let across = width.div_ceil(tw);
    let down = height.div_ceil(th);
    if offsets.len() != across * down || counts.len() != offsets.len() {
        return Err(err("tile count does not match the image"));
    }
    let Georef {
        lon0, lat0, dx, dy, ..
    } = georef_of(&ifd)?;
    let nodata = ifd
        .ascii(TAG_GDAL_NODATA)?
        .and_then(|s| s.parse::<f32>().ok());

    let mut data = vec![0f32; width * height];
    let mut tile = vec![0u8; tw * th * 4];
    let mut row = vec![0f32; tw];
    for (n, (&offset, &count)) in offsets.iter().zip(&counts).enumerate() {
        let start = offset as usize;
        let src = file
            .get(start..start.saturating_add(count as usize))
            .ok_or_else(|| err("tile outside the file"))?;
        if compression == 1 {
            if src.len() != tile.len() {
                return Err(err("uncompressed tile size"));
            }
            tile.copy_from_slice(src);
        } else {
            let mut z = flate2::read::ZlibDecoder::new(src);
            z.read_exact(&mut tile)
                .map_err(|e| err(&format!("tile {n}: {e}")))?;
        }
        let (tx, ty) = (n % across, n / across);
        for r in 0..th {
            let y = ty * th + r;
            let bytes = &mut tile[r * tw * 4..(r + 1) * tw * 4];
            if predictor == 3 {
                unpredict_row(bytes, &mut row);
            } else {
                for (i, v) in row.iter_mut().enumerate() {
                    *v = f32::from_le_bytes([
                        bytes[4 * i],
                        bytes[4 * i + 1],
                        bytes[4 * i + 2],
                        bytes[4 * i + 3],
                    ]);
                }
            }
            if y >= height {
                continue;
            }
            let x0 = tx * tw;
            let w = tw.min(width - x0);
            data[y * width + x0..y * width + x0 + w].copy_from_slice(&row[..w]);
        }
    }
    let mut missing = 0u64;
    for v in &mut data {
        if !v.is_finite() || Some(*v) == nodata {
            *v = 0.0;
            missing += 1;
        }
    }
    Ok(Raster {
        width,
        height,
        data,
        lon0,
        lat0,
        dx,
        dy,
        nodata: missing,
    })
}

/// Write a [`Raster`]-shaped GeoTIFF the way GLO-30 does (tiled, DEFLATE,
/// floating-point predictor, pixel is point): the tests' source cells.
pub fn encode(r: &Raster, tile: usize) -> Vec<u8> {
    use std::io::Write;
    let across = r.width.div_ceil(tile);
    let down = r.height.div_ceil(tile);
    let mut blobs = Vec::with_capacity(across * down);
    for ty in 0..down {
        for tx in 0..across {
            let mut raw = Vec::with_capacity(tile * tile * 4);
            for row in 0..tile {
                let y = (ty * tile + row).min(r.height - 1);
                let vals: Vec<f32> = (0..tile)
                    .map(|c| r.data[y * r.width + (tx * tile + c).min(r.width - 1)])
                    .collect();
                let mut planes = vec![0u8; tile * 4];
                for (i, v) in vals.iter().enumerate() {
                    let b = v.to_be_bytes();
                    for p in 0..4 {
                        planes[p * tile + i] = b[p];
                    }
                }
                for i in (1..planes.len()).rev() {
                    planes[i] = planes[i].wrapping_sub(planes[i - 1]);
                }
                raw.extend_from_slice(&planes);
            }
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            // Writing to a Vec cannot fail.
            let _ = z.write_all(&raw);
            blobs.push(z.finish().unwrap_or_default());
        }
    }
    // Header, then the IFD, then the out-of-line values, then the tiles.
    let short = |tag: u16, v: u16| (tag, 3u16, 1u32, Vec::from(v.to_le_bytes()));
    let mut fields: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        short(TAG_WIDTH, r.width as u16),
        short(TAG_HEIGHT, r.height as u16),
        short(TAG_BITS, 32),
        short(TAG_COMPRESSION, 8),
        short(262, 1),
        short(TAG_SAMPLES, 1),
        short(TAG_PLANAR, 1),
        short(TAG_PREDICTOR, 3),
        short(TAG_TILE_WIDTH, tile as u16),
        short(TAG_TILE_HEIGHT, tile as u16),
        (
            TAG_TILE_OFFSETS,
            4,
            blobs.len() as u32,
            vec![0; 4 * blobs.len()],
        ),
        (
            TAG_TILE_COUNTS,
            4,
            blobs.len() as u32,
            blobs
                .iter()
                .flat_map(|b| (b.len() as u32).to_le_bytes())
                .collect(),
        ),
        short(TAG_SAMPLE_FORMAT, 3),
        (
            TAG_PIXEL_SCALE,
            12,
            3,
            [r.dx, r.dy, 0.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
        (
            TAG_TIEPOINT,
            12,
            6,
            [0.0, 0.0, 0.0, r.lon0, r.lat0, 0.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
        (
            TAG_GEO_KEYS,
            3,
            8,
            [1u16, 1, 0, 1, KEY_RASTER_TYPE, 0, 1, 2]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
    ];
    let ifd_at = 8usize;
    let ifd_len = 2 + 12 * fields.len() + 4;
    let mut extra_at = ifd_at + ifd_len;
    let mut extra_len = 0usize;
    for f in &fields {
        if f.3.len() > 4 {
            extra_len += f.3.len();
        }
    }
    let mut tile_at = extra_at + extra_len;
    let mut offsets = Vec::with_capacity(blobs.len() * 4);
    for b in &blobs {
        offsets.extend_from_slice(&(tile_at as u32).to_le_bytes());
        tile_at += b.len();
    }
    for f in &mut fields {
        if f.0 == TAG_TILE_OFFSETS {
            f.3 = offsets.clone();
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"II*\0");
    out.extend_from_slice(&(ifd_at as u32).to_le_bytes());
    out.extend_from_slice(&(fields.len() as u16).to_le_bytes());
    let mut extra = Vec::with_capacity(extra_len);
    for (tag, typ, count, value) in &fields {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if value.len() <= 4 {
            let mut v = [0u8; 4];
            v[..value.len()].copy_from_slice(value);
            out.extend_from_slice(&v);
        } else {
            out.extend_from_slice(&(extra_at as u32).to_le_bytes());
            extra.extend_from_slice(value);
            extra_at += value.len();
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&extra);
    for b in &blobs {
        out.extend_from_slice(b);
    }
    out
}
