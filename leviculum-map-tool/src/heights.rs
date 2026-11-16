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
//! Web Mercator pixel and rounded to whole metres. The pixel centres come
//! from `leviculum_map::mercator`, the projection the map draws with: its
//! degrees x 10^7 are 1.1 cm, under a two-thousandth of a GLO-30 pixel, so
//! the cut needs no second, f64 projection of its own.
//!
//! Where GLO-30 has no cell the distribution says it is open sea, but it
//! also withholds some land cells; so a cut that meets an absent cell
//! fails, naming every absent cell of the box, unless
//! [`HeightOptions::absent_is_sea`] accepts them as 0 m. No-data pixels
//! inside a cell are written as 0 m and counted ([`HeightStats::nodata`]).
//! The output depends only on the source bytes, the box and the zooms: the
//! same input gives the same bytes, whatever the thread count.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use leviculum_map::elevation::{
    heights_to_planes, write_block, write_directory, DirectoryInfo, TILE_BYTES, TILE_CELLS,
    TILE_EDGE,
};
use leviculum_map::mercator;
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

    /// Whether the source has the cell, without fetching it whole.
    fn has(&self, lat: i32, lon: i32) -> Res<bool>;
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

    fn has(&self, lat: i32, lon: i32) -> Res<bool> {
        let name = glo30_name(lat, lon);
        Ok(self.0.join(format!("{name}.tif")).is_file()
            || self.0.join(&name).join(format!("{name}.tif")).is_file())
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

    fn has(&self, lat: i32, lon: i32) -> Res<bool> {
        let name = glo30_name(lat, lon);
        let url = format!("{}/{name}/{name}.tif", self.base);
        match self.agent.head(&url).call() {
            Ok(_) => Ok(true),
            Err(ureq::Error::StatusCode(404)) => Ok(false),
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
    /// Accept cells the source does not have as sea (0 m). Off, a cut
    /// meeting one fails before it writes a byte, naming every absent
    /// cell; GLO-30 withholds some land cells, which would read as sea.
    pub absent_is_sea: bool,
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
    /// The names of the cells the box needs that the source does not
    /// have, taken as sea level (only with [`HeightOptions::absent_is_sea`]).
    pub absent: Vec<String>,
    /// No-data or non-finite pixels in the fetched cells, written as 0 m.
    pub nodata: u64,
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

/// The world is 2^32 units of `leviculum_map::mercator` around.
const WORLD_UNITS: u64 = 1 << 32;

/// The longitude, degrees, of world x `v` (2^32 units, `v` up to the
/// world's east edge inclusive: 180).
fn lon_at(v: u64) -> f64 {
    if v >= WORLD_UNITS {
        return 180.0;
    }
    f64::from(mercator::lon_of_x(v as u32)) / 1e7
}

/// The latitude, degrees, of world y `v` (2^32 units, clamped to the
/// projection's south edge).
fn lat_at(v: u64) -> f64 {
    f64::from(mercator::lat_of_y(v.min(WORLD_UNITS - 1) as u32)) / 1e7
}

/// World units per tile at zoom `z`, and per pixel.
fn tile_units(z: u8) -> u64 {
    WORLD_UNITS >> z
}

/// The tile at zoom `z` holding a longitude; 180 is the last column.
fn tile_x(lon: f64, z: u8) -> u32 {
    let x = if lon >= 180.0 {
        WORLD_UNITS - 1
    } else {
        u64::from(mercator::x_of_lon(
            mercator::LatLon::from_degrees(0.0, lon).lon_e7,
        ))
    };
    (x / tile_units(z)) as u32
}

/// The tile at zoom `z` holding a latitude, clamped to the projection.
fn tile_y(lat: f64, z: u8) -> u32 {
    let y = mercator::y_of_lat(mercator::LatLon::from_degrees(lat, 0.0).lat_e7);
    (u64::from(y) / tile_units(z)) as u32
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
    let pixel = tile_units(zoom) / TILE_EDGE as u64;
    // The centre of pixel n of the tile row or column starting at `start`.
    let centre = |start: u64, n: usize| start + n as u64 * pixel + pixel / 2;
    let mut out = vec![0i16; TILE_CELLS];
    let lons: Vec<f64> = (0..TILE_EDGE)
        .map(|c| lon_at(centre(u64::from(tx) * tile_units(zoom), c)))
        .collect();
    for r in 0..TILE_EDGE {
        let lat = lat_at(centre(u64::from(ty) * tile_units(zoom), r));
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

/// The cells a block row needs: block row `y` at the block zoom (`block`
/// world units a block), columns `x0..=x1`.
fn row_cells(block: u64, x0: u32, x1: u32, y: u32) -> Vec<(i32, i32)> {
    let north = lat_at(u64::from(y) * block);
    let south = lat_at(u64::from(y + 1) * block);
    let west = lon_at(u64::from(x0) * block);
    let east = lon_at(u64::from(x1 + 1) * block);
    cells_for(north, south, west, east)
}

/// Cut the box into a height file written to `out`: the directory piece,
/// then one block piece per block meeting the box, north to south, west
/// to east. Every cell the box needs is probed first; an absent one fails
/// the cut before a byte is written, unless `options.absent_is_sea`.
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
    let block = tile_units(bz);

    // Every cell the box needs, probed before a byte is written.
    let mut absent = Vec::new();
    for y in y0..=y1 {
        for (la, lo) in row_cells(block, x0, x1, y) {
            let name = glo30_name(la, lo);
            if !absent.contains(&name) && !source.has(la, lo)? {
                absent.push(name);
            }
        }
    }
    if !absent.is_empty() && !options.absent_is_sea {
        return Err(format!(
            "the source has no cell {} (open sea, or land the source withholds); \
             accept them as 0 m with --absent-is-sea",
            absent.join(", ")
        )
        .into());
    }

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
        absent,
        ..HeightStats::default()
    };

    let side = 1u32 << (tz - bz);
    let block_world = f64::from(1u32 << bz);
    let mut mosaic = Mosaic {
        cells: HashMap::new(),
    };
    let threads = options.threads.max(1);
    for y in y0..=y1 {
        let mid = lat_at(u64::from(y) * block + block / 2).to_radians();
        let edge_km = EARTH_CIRCUMFERENCE_KM * libm::cos(mid) / block_world;
        stats.area_km2 += edge_km * edge_km * f64::from(x1 - x0 + 1);

        let needed = row_cells(block, x0, x1, y);
        mosaic.cells.retain(|k, _| needed.contains(k));
        for &(la, lo) in &needed {
            if mosaic.cells.contains_key(&(la, lo)) {
                continue;
            }
            let cell = match source.fetch(la, lo)? {
                Some(bytes) => {
                    stats.cells += 1;
                    stats.largest_fetch = stats.largest_fetch.max(bytes.len());
                    let raster = geotiff::decode(&bytes)
                        .map_err(|e| format!("{}: {e}", glo30_name(la, lo)))?;
                    stats.nodata += raster.nodata;
                    Some(raster)
                }
                None => {
                    // Probed above; a cell that vanished since is absent
                    // all the same, and only accepted as sea likewise.
                    let name = glo30_name(la, lo);
                    if !options.absent_is_sea {
                        return Err(format!("the source has no cell {name}").into());
                    }
                    if !stats.absent.contains(&name) {
                        stats.absent.push(name);
                    }
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
