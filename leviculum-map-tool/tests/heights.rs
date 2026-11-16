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
        nodata: 0,
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
        absent_is_sea: false,
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
        nodata: 0,
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
fn a_missing_cell_is_sea_level_when_accepted_and_named() {
    let dir = cells(&[]);
    let mut file = Vec::new();
    let stats = cut_heights(
        &DirSource(dir.path().to_owned()),
        &HeightOptions {
            absent_is_sea: true,
            ..options([3.0, 50.0, 3.001, 50.001], 1)
        },
        &mut file,
    )
    .expect("cut with absent cells accepted");
    // The box's z11 block reaches over four cells; every one is named.
    assert_eq!(
        stats.absent,
        [(49, 2), (49, 3), (50, 2), (50, 3)]
            .iter()
            .map(|&(lat, lon)| glo30_name(lat, lon))
            .collect::<Vec<_>>()
    );
    assert_eq!(stats.cells, 0);
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

/// Rewrite the IFD entry of `tag` in an [`geotiff::encode`]d file (IFD at
/// byte 8) as one LONG holding `value`.
fn patch_long(file: &mut [u8], tag: u16, value: u32) {
    let count = usize::from(u16::from_le_bytes([file[8], file[9]]));
    for n in 0..count {
        let e = 10 + 12 * n;
        if u16::from_le_bytes([file[e], file[e + 1]]) == tag {
            file[e + 2..e + 4].copy_from_slice(&4u16.to_le_bytes());
            file[e + 4..e + 8].copy_from_slice(&1u32.to_le_bytes());
            file[e + 8..e + 12].copy_from_slice(&value.to_le_bytes());
            return;
        }
    }
    panic!("tag {tag} not in the IFD");
}

/// The audit of 1740 (order 1758): a cell whose header claims huge
/// dimensions overflowed `width * height` (a panic in a debug build, a
/// wrapped allocation in release). It is refused with an error instead.
#[test]
fn audit_a_cell_claiming_huge_dimensions_is_refused_not_a_panic() {
    let raster = Raster {
        width: 100,
        height: 100,
        data: vec![1.0; 10_000],
        lon0: -4.0,
        lat0: 32.0,
        dx: 1.0 / 3600.0,
        dy: 1.0 / 3600.0,
        nodata: 0,
    };
    let mut file = geotiff::encode(&raster, 128);
    for tag in [256u16, 257, 322, 323] {
        patch_long(&mut file, tag, 0xFFFF_FFF0);
    }
    let got = std::panic::catch_unwind(|| geotiff::decode(&file).map(|r| r.width));
    match got {
        Ok(Err(e)) => assert!(e.to_string().contains("4294967280"), "{e}"),
        Ok(Ok(w)) => panic!("decoded a {w} pixel wide cell"),
        Err(_) => panic!("decode panicked"),
    }
}

/// The audit of 1740 (order 1758): a land cell the source does not have
/// was cut as 0 m without a word. The cut now fails, naming the cell.
#[test]
fn a_missing_cell_is_refused_by_name() {
    let dir = cells(&[]);
    let mut out = Vec::new();
    let got = cut_heights(
        &DirSource(dir.path().to_owned()),
        &options([3.0, 50.0, 3.001, 50.001], 1),
        &mut out,
    );
    let message = got.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains(&glo30_name(50, 3)),
        "the absent cell is named: {message:?}"
    );
}

/// A whole box of absent cells is refused naming each of them, before a
/// byte is written.
#[test]
fn every_absent_cell_is_named_and_nothing_written() {
    let dir = cells(&[(44, 9)]);
    let mut out = Vec::new();
    let got = cut_heights(
        &DirSource(dir.path().to_owned()),
        &options(CORNER, 1),
        &mut out,
    );
    let message = got.err().map(|e| e.to_string()).unwrap_or_default();
    for (lat, lon) in [(44, 10), (45, 9), (45, 10)] {
        assert!(message.contains(&glo30_name(lat, lon)), "{message}");
    }
    assert!(!message.contains(&glo30_name(44, 9)), "{message}");
    assert!(out.is_empty(), "nothing written before the refusal");
}

/// No-data pixels are written as 0 m but counted, by the decoder and in
/// the cut's statistics.
#[test]
fn nodata_pixels_are_counted() {
    let dir = cells(&[(44, 9), (44, 10), (45, 10)]);
    // Cell (45, 9) with seven NaN pixels far from the box.
    let d = 1.0 / N as f64;
    let mut data = vec![100f32; N * N];
    for i in 0..7 {
        data[N * 10 + i] = f32::NAN;
    }
    let raster = Raster {
        width: N,
        height: N,
        data,
        lon0: 9.0,
        lat0: 46.0,
        dx: d,
        dy: d,
        nodata: 0,
    };
    let bytes = geotiff::encode(&raster, 128);
    assert_eq!(geotiff::decode(&bytes).expect("decode").nodata, 7);
    std::fs::write(dir.path().join(format!("{}.tif", glo30_name(45, 9))), bytes).expect("write");
    let mut out = Vec::new();
    let stats = cut_heights(
        &DirSource(dir.path().to_owned()),
        &options(CORNER, 1),
        &mut out,
    )
    .expect("cut");
    assert_eq!(stats.nodata, 7);
    assert!(stats.absent.is_empty());
}

/// A huge cell met by a cut fails the cut with the cell's name.
#[test]
fn a_cell_claiming_huge_dimensions_fails_the_cut_by_name() {
    let dir = cells(&[(44, 9), (44, 10), (45, 9), (45, 10)]);
    let path = dir.path().join(format!("{}.tif", glo30_name(45, 10)));
    let mut bytes = std::fs::read(&path).expect("read cell");
    patch_long(&mut bytes, 256, 0xFFFF_FFF0);
    std::fs::write(&path, bytes).expect("write cell");
    let mut out = Vec::new();
    let got = cut_heights(
        &DirSource(dir.path().to_owned()),
        &options(CORNER, 1),
        &mut out,
    );
    let message = got.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains(&glo30_name(45, 10)) && message.contains("4294967280"),
        "{message}"
    );
}

/// The header of a real GLO-30 cell, `Copernicus_DSM_COG_10_N31_00_W004_00_DEM`,
/// cut after its first IFD's values (fixtures/README.md): its pixels sit
/// where the cell's own metadata (`Copernicus_DSM_10_N31_00_W004_00.xml`)
/// puts them. The XML names the upper-left corner -4.00000, 32.00000 and a
/// 1.0 arcsec spacing for rows and columns; the GeoTIFF says pixel is
/// point, so pixel (0, 0)'s centre is that corner. A pixel-is-area shift
/// or a misread tiepoint moves lon0 or lat0 by half a pixel (1.4e-4°).
#[test]
fn a_real_glo30_header_places_its_pixels_where_its_metadata_says() {
    let head = include_bytes!("fixtures/Copernicus_DSM_COG_10_N31_00_W004_00_DEM.head.tif");
    let g = geotiff::georef(head).expect("the header reads");
    assert_eq!((g.width, g.height), (3600, 3600));
    let arcsec = 1.0 / 3600.0;
    assert!((g.dx - arcsec).abs() < 1e-12, "dx {}", g.dx);
    assert!((g.dy - arcsec).abs() < 1e-12, "dy {}", g.dy);
    assert!((g.lon0 - -4.0).abs() < 1e-9, "lon0 {}", g.lon0);
    assert!((g.lat0 - 32.0).abs() < 1e-9, "lat0 {}", g.lat0);
}
