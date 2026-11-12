//! The height cut (order 1740) over synthetic GLO-30-shaped cells: tiled,
//! DEFLATE, floating-point predictor, pixel is point, written by the same
//! layout the bucket uses. The cut is deterministic (same input, same
//! bytes, whatever the thread count), samples a surface it can be checked
//! against, reaches across cell edges, and takes a missing cell as sea.

use std::path::Path;

use leviculum_map::elevation::{split, Block, TileDecoder, TILE_CELLS, TILE_EDGE};
use leviculum_map_tool::geotiff::{self, Raster};
use leviculum_map_tool::heights::{
    cut_heights, glo30_name, DirSource, HeightOptions, ATTRIBUTION, NOTICE,
};

/// Cells of 360 x 360 pixels (10" spacing), pixel (0, 0) centred on the
/// cell's north-west corner, the last row and column one pixel short of the
/// south and east edges: GLO-30's layout at a tenth of its resolution.
const N: usize = 360;

/// A plane in degrees: exact under bilinear sampling, so every cut height
/// can be predicted.
fn surface(lon: f64, lat: f64) -> f64 {
    500.0 + 300.0 * (lon - 9.0) + 200.0 * (lat - 44.0)
}

fn write_cell(dir: &Path, lat: i32, lon: i32) {
    let d = 1.0 / N as f64;
    let (lon0, lat0) = (f64::from(lon), f64::from(lat + 1));
    let mut data = vec![0f32; N * N];
    for r in 0..N {
        for c in 0..N {
            data[r * N + c] = surface(lon0 + c as f64 * d, lat0 - r as f64 * d) as f32;
        }
    }
    let raster = Raster {
        width: N,
        height: N,
        data,
        lon0,
        lat0,
        dx: d,
        dy: d,
    };
    let bytes = geotiff::encode(&raster, 128);
    std::fs::write(dir.join(format!("{}.tif", glo30_name(lat, lon))), bytes).expect("write cell");
}

fn cells(present: &[(i32, i32)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for &(lat, lon) in present {
        write_cell(dir.path(), lat, lon);
    }
    dir
}

fn options(bbox: [f64; 4], threads: usize) -> HeightOptions {
    HeightOptions {
        bbox,
        tile_zoom: 12,
        block_zoom: 11,
        level: 19,
        threads,
    }
}

fn cut(dir: &Path, bbox: [f64; 4], threads: usize) -> Vec<u8> {
    let mut out = Vec::new();
    cut_heights(
        &DirSource(dir.to_owned()),
        &options(bbox, threads),
        &mut out,
    )
    .expect("cut");
    out
}

/// The centre of z12 pixel (gx, gy), degrees, computed here on its own.
fn centre(gx: u32, gy: u32) -> (f64, f64) {
    let world = (TILE_EDGE as f64) * 4096.0;
    let lon = (f64::from(gx) + 0.5) / world * 360.0 - 180.0;
    let y = std::f64::consts::PI * (1.0 - 2.0 * (f64::from(gy) + 0.5) / world);
    (lon, y.sinh().atan().to_degrees())
}

/// A box around (10 E, 45 N): its z11 block reaches across the corner of
/// four cells.
const CORNER: [f64; 4] = [9.999, 44.999, 10.001, 45.001];

#[test]
fn a_geotiff_round_trips_through_the_glo30_layout() {
    let raster = Raster {
        width: 300,
        height: 200,
        data: (0..60_000).map(|i| (i as f32) * 0.37 - 900.5).collect(),
        lon0: -4.0,
        lat0: 32.0,
        dx: 1.0 / 3600.0,
        dy: 1.0 / 3600.0,
    };
    let back = geotiff::decode(&geotiff::encode(&raster, 128)).expect("decode");
    assert_eq!((back.width, back.height), (300, 200));
    assert_eq!(back.data, raster.data);
    assert_eq!((back.lon0, back.lat0), (-4.0, 32.0));
    assert_eq!((back.dx, back.dy), (1.0 / 3600.0, 1.0 / 3600.0));
}

#[test]
fn the_cut_is_deterministic_whatever_the_thread_count() {
    let dir = cells(&[(44, 9), (44, 10), (45, 9), (45, 10)]);
    let one = cut(dir.path(), CORNER, 1);
    let again = cut(dir.path(), CORNER, 1);
    let many = cut(dir.path(), CORNER, 3);
    assert!(one.len() > 1000);
    assert_eq!(one, again, "same input, same bytes");
    assert_eq!(one, many, "the thread count does not show in the bytes");
}

#[test]
fn the_cut_samples_the_surface_across_cell_edges() {
    let dir = cells(&[(44, 9), (44, 10), (45, 9), (45, 10)]);
    let file = cut(dir.path(), CORNER, 2);
    let (directory, ranges) = split(&file).expect("split");
    assert_eq!(directory.attribution(), ATTRIBUTION);
    assert_eq!(directory.notice(), NOTICE);
    assert_eq!(directory.len(), 1, "the box lies in one z11 block");
    let block = Block::parse(&file[ranges[1].clone()]).expect("block");
    let (bx, by) = block.position();
    let mut decoder = TileDecoder::new();
    let mut out = vec![0i16; TILE_CELLS];
    let mut scratch = vec![0i16; TILE_CELLS];
    let (mut north, mut south, mut west, mut east) = (false, false, false, false);
    for ty in by * 2..by * 2 + 2 {
        for tx in bx * 2..bx * 2 + 2 {
            block
                .heights(&mut decoder, 12, tx, ty, &mut out, &mut scratch)
                .expect("decode");
            for r in 0..TILE_EDGE {
                for c in 0..TILE_EDGE {
                    let gx = tx * TILE_EDGE as u32 + c as u32;
                    let gy = ty * TILE_EDGE as u32 + r as u32;
                    let (lon, lat) = centre(gx, gy);
                    north |= lat > 45.0;
                    south |= lat < 45.0;
                    west |= lon < 10.0;
                    east |= lon > 10.0;
                    let want = surface(lon, lat);
                    let got = f64::from(out[r * TILE_EDGE + c]);
                    assert!(
                        (got - want).abs() <= 0.5 + 1e-3,
                        "{lon:.5},{lat:.5}: {got} against {want:.3}"
                    );
                }
            }
        }
    }
    assert!(north && south && west && east, "the block spans the corner");
}

#[test]
fn a_missing_cell_is_sea_level() {
    let dir = cells(&[]);
    let file = cut(dir.path(), [3.0, 50.0, 3.001, 50.001], 1);
    let (_, ranges) = split(&file).expect("split");
    let block = Block::parse(&file[ranges[1].clone()]).expect("block");
    let (bx, by) = block.position();
    let mut decoder = TileDecoder::new();
    let mut out = vec![1i16; TILE_CELLS];
    let mut scratch = vec![0i16; TILE_CELLS];
    block
        .heights(&mut decoder, 11, bx, by, &mut out, &mut scratch)
        .expect("derive");
    assert!(out.iter().all(|&h| h == 0));
}
