//! The height cut (order 1740, K6a): Copernicus GLO-30 for a box into the
//! height pieces of `leviculum_map::elevation`, z12 tiles in z9 blocks.
//!
//! Streaming: the cut walks the blocks north to south a row at a time and
//! holds only the source cells that row and its south neighbour need. A
//! cell is fetched into memory (HTTP, or read from a local directory),
//! decoded, used while any block of the current row needs it and dropped
//! after; nothing is written to disk but the output, so the peak disk is
//! the output file. The peak memory is the cells of one block row: at
//! 30 N a z9 row spans under a degree of latitude, so two cell rows of
//! the box's width, 52 MB a cell decoded.
//!
//! Each height is the source bilinearly sampled at the centre of its z12
//! Web Mercator pixel and rounded to whole metres. Where GLO-30 has no
//! cell (open sea, as the distribution says) the height is 0. The output
//! depends only on the source bytes, the box and the zooms: the same
//! input gives the same bytes, whatever the thread count.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use leviculum_map::elevation::{
    heights_to_planes, write_block, write_directory, DirectoryInfo, TILE_BYTES, TILE_CELLS,
    TILE_EDGE,
};
use leviculum_mapcut::Res;

use crate::geotiff::{self, Raster};

/// The attribution line, the licence's own words for adapted data
/// (Article 6 (b) of the licence the source ships as `INFO/eula_F.pdf`
/// beside every cell).
pub const ATTRIBUTION: &str = "produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and \
     © Airbus Defence and Space GmbH 2014-2018 provided under COPERNICUS by the European Union \
     and ESA; all rights reserved";

/// The notice: the licence named, the disclaimer Article 6 (c) of it
/// requires word for word, and the source.
pub const NOTICE: &str = "Heights from the Copernicus DEM GLO-30 Public (COP-DEM-GLO-30-F), \
     under the Licence for the use of the Copernicus WorldDEM-30, free of charge, \
     https://spacedata.copernicus.eu/collections/copernicus-digital-elevation-model . \
     The organisations in charge of the Copernicus programme by law or by delegation do not \
     incur any liability for any use of the Copernicus WorldDEM-30. Resampled to Web Mercator \
     and rounded to whole metres; no endorsement by the provider or the licensor is implied.";

/// The public GLO-30 bucket.
pub const GLO30_HTTP: &str = "https://copernicus-dem-30m.s3.amazonaws.com";

/// The GLO-30 object name of the cell whose south-west corner is
/// (lat, lon), as the bucket names it.
pub fn glo30_name(lat: i32, lon: i32) -> String {
    let ns = if lat < 0 { 'S' } else { 'N' };
    let ew = if lon < 0 { 'W' } else { 'E' };
    format!(
        "Copernicus_DSM_COG_10_{ns}{:02}_00_{ew}{:03}_00_DEM",
        lat.unsigned_abs(),
        lon.unsigned_abs()
    )
}

/// Where the cut reads GLO-30 cells from.
pub trait DemSource: Sync {
    /// The GeoTIFF bytes of the cell whose south-west corner is (lat, lon),
    /// or `None` where the source has no cell (open sea).
    fn fetch(&self, lat: i32, lon: i32) -> Res<Option<Vec<u8>>>;
}

/// Cells in a local directory, as `<name>.tif` or `<name>/<name>.tif`.
pub struct DirSource(pub PathBuf);

impl DemSource for DirSource {
    fn fetch(&self, lat: i32, lon: i32) -> Res<Option<Vec<u8>>> {
        let name = glo30_name(lat, lon);
        for path in [
            self.0.join(format!("{name}.tif")),
            self.0.join(&name).join(format!("{name}.tif")),
        ] {
            match std::fs::read(&path) {
                Ok(b) => return Ok(Some(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display()).into()),
            }
        }
        Ok(None)
    }
}

/// Cells fetched over HTTP into memory: `<base>/<name>/<name>.tif`. A 404
/// is a cell the source does not have.
pub struct HttpSource {
    agent: ureq::Agent,
    base: String,
}

impl HttpSource {
    /// A source at `base` (e.g. [`GLO30_HTTP`]).
    pub fn new(base: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(600)))
            .user_agent(concat!("leviculum-map-tool/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        HttpSource {
            agent,
            base: base.trim_end_matches('/').to_owned(),
        }
    }
}

impl DemSource for HttpSource {
    fn fetch(&self, lat: i32, lon: i32) -> Res<Option<Vec<u8>>> {
        let name = glo30_name(lat, lon);
        let url = format!("{}/{name}/{name}.tif", self.base);
        match self.agent.get(&url).call() {
            Ok(mut response) => Ok(Some(
                response
                    .body_mut()
                    .with_config()
                    .limit(256 << 20)
                    .read_to_vec()
                    .map_err(|e| format!("{url}: {e}"))?,
            )),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(format!("{url}: {e}").into()),
        }
    }
}

/// What to cut.
#[derive(Clone, Debug)]
pub struct HeightOptions {
    /// west, south, east, north in degrees.
    pub bbox: [f64; 4],
    /// The zoom the tiles are stored at.
    pub tile_zoom: u8,
    /// The zoom one block is a tile of.
    pub block_zoom: u8,
    /// zstd level of the tile frames.
    pub level: i32,
    /// Worker threads; the output does not depend on it.
    pub threads: usize,
}

/// What the cut did.
#[derive(Clone, Debug, Default)]
pub struct HeightStats {
    /// Blocks written.
    pub blocks: usize,
    /// Tiles written.
    pub tiles: usize,
    /// Bytes written, directory included.
    pub bytes: u64,
    /// Source cells fetched.
    pub cells: usize,
    /// Source cells the source does not have (taken as sea level).
    pub absent: usize,
    /// The most source cells held at once.
    pub peak_cells: usize,
    /// The bytes of the largest source cell fetched (the most a fetch holds
    /// before decoding).
    pub largest_fetch: usize,
    /// The ground area the blocks cover, km², at each block row's mean
    /// latitude.
    pub area_km2: f64,
    /// Seconds spent compressing tiles, summed over the threads.
    pub encode_seconds: f64,
}

const EARTH_CIRCUMFERENCE_KM: f64 = 40_075.016_686;

fn lon_of(px: f64, world: f64) -> f64 {
    px / world * 360.0 - 180.0
}

fn lat_of(py: f64, world: f64) -> f64 {
    libm::atan(libm::sinh(core::f64::consts::PI * (1.0 - 2.0 * py / world))).to_degrees()
}

fn tile_x(lon: f64, z: u8) -> u32 {
    let n = f64::from(1u32 << z);
    (((lon + 180.0) / 360.0 * n).floor()).clamp(0.0, n - 1.0) as u32
}

fn tile_y(lat: f64, z: u8) -> u32 {
    let n = f64::from(1u32 << z);
    let r = lat.to_radians();
    let y = (1.0 - libm::log(libm::tan(r) + 1.0 / libm::cos(r)) / core::f64::consts::PI) / 2.0 * n;
    y.floor().clamp(0.0, n - 1.0) as u32
}

/// The decoded cells a block row needs; `None` for a cell the source does
/// not have.
struct Mosaic {
    cells: HashMap<(i32, i32), Option<Raster>>,
}

impl Mosaic {
    fn cell(&self, la: i32, lo: i32) -> Option<&Raster> {
        self.cells.get(&(la, lo)).and_then(Option::as_ref)
    }

    /// The height at row `r`, column `c` of cell (la, lo), reaching into
    /// the cell south of it or east of it for the bilinear's far corners.
    fn at(&self, la: i32, lo: i32, r: usize, c: usize) -> f64 {
        let Some(cell) = self.cell(la, lo) else {
            return 0.0;
        };
        if r >= cell.height {
            // The south neighbour's row 0 is this cell's row `height`; its
            // columns may be spaced differently, so map by longitude.
            let lon = cell.lon0 + c as f64 * cell.dx;
            let Some(south) = self.cell(la - 1, lo) else {
                return 0.0;
            };
            let c2 = ((lon - south.lon0) / south.dx).round().max(0.0) as usize;
            return self.at(la - 1, lo, r - cell.height, c2.min(south.width));
        }
        if c >= cell.width {
            return self.at(la, lo + 1, r, c - cell.width);
        }
        f64::from(cell.data[r * cell.width + c])
    }

    /// The source bilinearly sampled at (lon, lat).
    fn sample(&self, lon: f64, lat: f64) -> f64 {
        // A cell's pixel centres run from its north edge down to just above
        // its south edge, and from its west edge to just short of its east.
        let la = lat.ceil() as i32 - 1;
        let lo = lon.floor() as i32;
        let Some(cell) = self.cell(la, lo) else {
            return 0.0;
        };
        let fr = ((cell.lat0 - lat) / cell.dy).max(0.0);
        let fc = ((lon - cell.lon0) / cell.dx).max(0.0);
        let (r0, c0) = (fr.floor() as usize, fc.floor() as usize);
        let (wr, wc) = (fr - r0 as f64, fc - c0 as f64);
        let a = self.at(la, lo, r0, c0);
        let b = self.at(la, lo, r0, c0 + 1);
        let c = self.at(la, lo, r0 + 1, c0);
        let d = self.at(la, lo, r0 + 1, c0 + 1);
        (a * (1.0 - wc) + b * wc) * (1.0 - wr) + (c * (1.0 - wc) + d * wc) * wr
    }
}

/// One tile's heights, sampled from the mosaic.
fn sample_tile(mosaic: &Mosaic, zoom: u8, tx: u32, ty: u32) -> Vec<i16> {
    let world = f64::from(TILE_EDGE as u32) * f64::from(1u32 << zoom);
    let mut out = vec![0i16; TILE_CELLS];
    let lons: Vec<f64> = (0..TILE_EDGE)
        .map(|c| lon_of(f64::from(tx) * TILE_EDGE as f64 + c as f64 + 0.5, world))
        .collect();
    for r in 0..TILE_EDGE {
        let lat = lat_of(f64::from(ty) * TILE_EDGE as f64 + r as f64 + 0.5, world);
        for (c, &lon) in lons.iter().enumerate() {
            let h = mosaic.sample(lon, lat).round();
            out[r * TILE_EDGE + c] = h.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        }
    }
    out
}

/// One tile's frame and the seconds its compression took.
type Encoded = (Vec<u8>, f64);

/// Compress one tile's heights into its zstd frame, with the content
/// checksum the reader requires.
pub fn compress_tile(heights: &[i16], level: i32) -> Res<Vec<u8>> {
    let mut planes = vec![0u8; TILE_BYTES];
    heights_to_planes(heights, &mut planes);
    let mut c = zstd::bulk::Compressor::new(level)?;
    c.include_checksum(true)?;
    c.include_contentsize(true)?;
    Ok(c.compress(&planes)?)
}

/// How far past a block row's edge the bilinear reaches into the source:
/// one GLO-30 pixel (1"), taken twice for rounding.
const REACH_DEGREES: f64 = 2.0 / 3600.0;

/// The cells (south-west corners) a block row from latitude `north` down
/// to `south` and longitude `west` to `east` needs, with the bilinear's
/// reach past its south and east edges.
fn cells_for(north: f64, south: f64, west: f64, east: f64) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let top = north.ceil() as i32 - 1;
    let bottom = (south - REACH_DEGREES).ceil() as i32 - 1;
    for la in bottom..=top {
        for lo in (west.floor() as i32)..=((east + REACH_DEGREES).floor() as i32) {
            if (-90..90).contains(&la) && (-180..180).contains(&lo) {
                out.push((la, lo));
            }
        }
    }
    out
}

/// Cut the box into a height file written to `out`: the directory piece,
/// then one block piece per block meeting the box, north to south, west
/// to east.
pub fn cut_heights(
    source: &dyn DemSource,
    options: &HeightOptions,
    out: &mut dyn Write,
) -> Res<HeightStats> {
    let [west, south, east, north] = options.bbox;
    if !(west < east && south < north && (-180.0..=180.0).contains(&west)) {
        return Err("bbox: west,south,east,north with west < east and south < north".into());
    }
    let bz = options.block_zoom;
    let tz = options.tile_zoom;
    let (x0, x1) = (tile_x(west, bz), tile_x(east, bz));
    let (y0, y1) = (tile_y(north, bz), tile_y(south, bz));
    let mut blocks = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            blocks.push((x, y));
        }
    }
    let e7 = |v: f64| (v * 1e7).round() as i32;
    let directory = write_directory(
        &DirectoryInfo {
            tile_zoom: tz,
            block_zoom: bz,
            bounds: [e7(west), e7(south), e7(east), e7(north)],
            attribution: ATTRIBUTION,
            notice: NOTICE,
        },
        &blocks,
    )
    .map_err(|e| format!("directory: {e:?}"))?;
    out.write_all(&directory)?;
    let mut stats = HeightStats {
        bytes: directory.len() as u64,
        ..HeightStats::default()
    };

    let side = 1u32 << (tz - bz);
    let block_world = f64::from(1u32 << bz);
    let mut mosaic = Mosaic {
        cells: HashMap::new(),
    };
    let threads = options.threads.max(1);
    for y in y0..=y1 {
        // Pixel centres of this block row, with half a pixel's margin.
        let north_lat = lat_of(f64::from(y), block_world);
        let south_lat = lat_of(f64::from(y + 1), block_world);
        let west_lon = lon_of(f64::from(x0), block_world);
        let east_lon = lon_of(f64::from(x1 + 1), block_world);
        let mid = lat_of(f64::from(y) + 0.5, block_world).to_radians();
        let edge_km = EARTH_CIRCUMFERENCE_KM * libm::cos(mid) / block_world;
        stats.area_km2 += edge_km * edge_km * f64::from(x1 - x0 + 1);

        let needed = cells_for(north_lat, south_lat, west_lon, east_lon);
        mosaic.cells.retain(|k, _| needed.contains(k));
        for &(la, lo) in &needed {
            if mosaic.cells.contains_key(&(la, lo)) {
                continue;
            }
            let cell = match source.fetch(la, lo)? {
                Some(bytes) => {
                    stats.cells += 1;
                    stats.largest_fetch = stats.largest_fetch.max(bytes.len());
                    Some(
                        geotiff::decode(&bytes)
                            .map_err(|e| format!("{}: {e}", glo30_name(la, lo)))?,
                    )
                }
                None => {
                    stats.absent += 1;
                    None
                }
            };
            mosaic.cells.insert((la, lo), cell);
        }
        stats.peak_cells = stats
            .peak_cells
            .max(mosaic.cells.values().filter(|c| c.is_some()).count());

        for x in x0..=x1 {
            let count = (side * side) as usize;
            let mut frames: Vec<Option<Res<Encoded>>> = (0..count).map(|_| None).collect();
            std::thread::scope(|s| {
                let mosaic = &mosaic;
                let handles: Vec<_> = (0..threads)
                    .map(|t| {
                        s.spawn(move || {
                            let mut mine = Vec::new();
                            for i in (t..count).step_by(threads) {
                                let (tx, ty) =
                                    (x * side + i as u32 % side, y * side + i as u32 / side);
                                let heights = sample_tile(mosaic, tz, tx, ty);
                                let t0 = std::time::Instant::now();
                                let frame = compress_tile(&heights, options.level)
                                    .map(|f| (f, t0.elapsed().as_secs_f64()));
                                mine.push((i, frame));
                            }
                            mine
                        })
                    })
                    .collect();
                for h in handles {
                    if let Ok(mine) = h.join() {
                        for (i, frame) in mine {
                            frames[i] = Some(frame);
                        }
                    }
                }
            });
            let mut done = Vec::with_capacity(count);
            for (i, f) in frames.into_iter().enumerate() {
                let (frame, secs) = f.ok_or_else(|| format!("tile {i}: a worker died"))??;
                stats.encode_seconds += secs;
                done.push(frame);
            }
            let piece = write_block(tz, bz, x, y, &done).map_err(|e| format!("block: {e:?}"))?;
            out.write_all(&piece)?;
            stats.blocks += 1;
            stats.tiles += count;
            stats.bytes += piece.len() as u64;
        }
    }
    Ok(stats)
}
