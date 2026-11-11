//! The geoid table: its size and bytes pinned, its posts against the 5'
//! source, and its heights against the full 1' EGM2008.

use leviculum_map::geoid::{
    height_above_sea_level, height_above_sea_level_cm, undulation, undulation_cm, Grid, EGM2008_15,
};

/// 20 header bytes and 721 x 1440 u16 posts.
const TABLE_BYTES: usize = 2_076_500;

/// FNV-1a 64 of the whole table, as `scripts/bake-geoid.py` wrote it
/// (sha256 e92f47c5598739ea2b30cf21405d8bdfb0b06f2dabe9afac4f6d0ccdf57f1ebd).
const TABLE_FNV1A64: u64 = 0x22b5_1176_1f35_2163;

/// (row, col, post) of the 15' table read straight from GeographicLib's
/// 5' `egm2008-5.pgm` (sha256 96d55e88...b34a3) at row 3 * row, column
/// 3 * col, printed by `scripts/bake-geoid.py`: the four corners, then 60
/// spread by a fixed LCG.
const SOURCE_POSTS: [(usize, usize, u16); 64] = [
    (0, 0, 40966),
    (0, 1439, 40966),
    (720, 0, 25950),
    (720, 1439, 25950),
    (604, 95, 44652),
    (248, 1013, 28074),
    (252, 539, 48020),
    (21, 753, 39163),
    (393, 1367, 38695),
    (617, 877, 22492),
    (336, 691, 43992),
    (558, 1385, 44691),
    (603, 527, 24778),
    (264, 1061, 25481),
    (567, 1355, 42590),
    (669, 673, 17808),
    (246, 647, 37170),
    (317, 1245, 22473),
    (218, 835, 29131),
    (310, 313, 6077),
    (494, 383, 26554),
    (588, 789, 25041),
    (677, 827, 18525),
    (86, 209, 36751),
    (78, 1431, 53132),
    (399, 429, 33305),
    (552, 499, 27794),
    (697, 1001, 25360),
    (482, 207, 41140),
    (107, 357, 27314),
    (276, 1067, 29153),
    (634, 705, 16344),
    (586, 103, 46050),
    (164, 317, 21661),
    (61, 355, 33421),
    (59, 857, 34337),
    (237, 639, 37120),
    (155, 917, 30644),
    (574, 1147, 40227),
    (340, 17, 42330),
    (193, 151, 42921),
    (415, 1197, 41789),
    (275, 211, 24288),
    (251, 1417, 47492),
    (31, 1135, 42422),
    (457, 325, 26659),
    (98, 683, 38725),
    (376, 161, 25376),
    (365, 455, 51412),
    (119, 221, 36058),
    (30, 515, 37440),
    (135, 665, 39045),
    (430, 1343, 33938),
    (552, 277, 49282),
    (152, 923, 31362),
    (714, 1201, 26695),
    (116, 823, 40815),
    (717, 621, 25879),
    (210, 1075, 25774),
    (33, 41, 46078),
    (115, 751, 38573),
    (66, 325, 33179),
    (618, 875, 22216),
    (399, 257, 19316),
];

/// Reference undulations in metres from GeographicLib's `GeoidEval`
/// (geographiclib-tools 2.x, Debian) on the full-resolution 1' EGM2008
/// grid `egm2008-1.pgm` (tarball sha256
/// bdb382d0be7ece9142450eacc24b7b7f0889ee3e0ba4f535b04ec383f94c0fb5),
/// cubic interpolation, its stated error 3 mm:
/// `GeoidEval -n egm2008-1 -d <dir>`, one "lat lon" per line.
///
/// Tolerance: the 15' table is bilinear between posts 15' apart, the
/// source 1' grid sees every bump between them. Measured against
/// `GeoidEval` at every 15' cell centre of the earth (1,036,800 points,
/// where bilinear errs most): rms 0.10 m, 99.9 % within 0.79 m, 99.99 %
/// within 1.23 m, the largest 2.67 m at 10.875 N 73.625 W, the Sierra
/// Nevada de Santa Marta, where the geoid bends hardest. 1.5 m holds the
/// sharp New Guinea peak, which bilinear flattens by 1.07 m, with margin;
/// the Santa Marta worst case gets 3 m and pins the measured maximum.
const REFERENCE: [(&str, f64, f64, f64, f64); 11] = [
    ("Frankfurt", 50.1109, 8.6821, 48.0703, 1.5),
    (
        "Indian Ocean low, south of Sri Lanka",
        4.7,
        78.8,
        -106.8991,
        1.5,
    ),
    ("New Guinea high", -8.4, 147.4, 85.8001, 1.5),
    ("north pole", 90.0, 0.0, 14.8980, 1.5),
    ("south pole", -90.0, 0.0, -30.1500, 1.5),
    ("equator on the antimeridian", 0.0, 180.0, 21.2813, 1.5),
    ("Fiji, on the antimeridian", -17.75, 180.0, 49.9919, 1.5),
    ("Fiji, just west of it", -17.75, -179.99, 49.9866, 1.5),
    ("Mount Everest", 27.988, 86.925, -28.4951, 1.5),
    ("Sydney", -33.8688, 151.2093, 22.3574, 1.5),
    (
        "Sierra Nevada de Santa Marta, worst cell",
        10.875,
        -73.625,
        9.9157,
        3.0,
    ),
];

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[test]
fn table_size_and_bytes_are_pinned() {
    assert_eq!(EGM2008_15.len(), TABLE_BYTES);
    assert_eq!(fnv1a64(EGM2008_15), TABLE_FNV1A64);
    let grid = Grid::new(EGM2008_15).expect("table reads");
    assert_eq!((grid.rows(), grid.cols()), (721, 1440));
}

#[test]
fn posts_equal_the_source_posts() {
    let grid = Grid::new(EGM2008_15).expect("table reads");
    for (row, col, post) in SOURCE_POSTS {
        assert_eq!(grid.post(row, col), Some(post), "row {row} col {col}");
    }
    // A pole is one point: every post of its row is the same.
    for row in [0, 720] {
        let first = grid.post(row, 0);
        for col in 0..1440 {
            assert_eq!(grid.post(row, col), first, "row {row} col {col}");
        }
    }
    assert_eq!(grid.post(721, 0), None);
    assert_eq!(grid.post(0, 1440), None);
}

#[test]
fn undulation_matches_the_1_minute_model() {
    for (name, lat, lon, reference, tolerance) in REFERENCE {
        let ours = undulation(lat, lon);
        let off = ours - reference;
        assert!(
            off.abs() <= tolerance,
            "{name}: ours {ours:.4} m, EGM2008 1' {reference:.4} m, off {off:.4} m"
        );
    }
}

#[test]
fn a_post_is_returned_exactly() {
    // Frankfurt's nearest post, 50.0 N 8.75 E: row 160, col 35.
    let grid = Grid::new(EGM2008_15).expect("table reads");
    let post = grid.post_m(160, 35).expect("post");
    assert!((undulation(50.0, 8.75) - post).abs() < 1e-9);
    // The poles hold their posts whatever the longitude.
    assert!((undulation(90.0, 123.0) - 14.898).abs() < 1e-9);
    assert!((undulation(-90.0, -45.0) - -30.15).abs() < 1e-9);
}

#[test]
fn longitude_wraps_and_latitude_clamps() {
    for (lat, lon) in [(50.1109, 8.6821), (-17.75, 179.99), (4.7, 78.8)] {
        let n = undulation(lat, lon);
        assert!((undulation(lat, lon + 360.0) - n).abs() < 1e-9);
        assert!((undulation(lat, lon - 360.0) - n).abs() < 1e-9);
        assert!((undulation(lat, lon + 720.0) - n).abs() < 1e-9);
    }
    // 180 E and 180 W are one meridian, and so are 0 and 360.
    assert!((undulation(-17.75, 180.0) - undulation(-17.75, -180.0)).abs() < 1e-9);
    assert!((undulation(12.3, 0.0) - undulation(12.3, 360.0)).abs() < 1e-9);
    // Continuous across the last column back to the first.
    let west = undulation(30.0, 359.999_999);
    let east = undulation(30.0, 0.0);
    assert!((west - east).abs() < 1e-3, "{west} vs {east}");
    // Beyond a pole is the pole.
    assert!((undulation(95.0, 10.0) - undulation(90.0, 10.0)).abs() < 1e-9);
    assert!((undulation(-91.0, 10.0) - undulation(-90.0, 10.0)).abs() < 1e-9);
}

#[test]
fn height_above_sea_level_subtracts_the_undulation() {
    // A GNSS fix in Frankfurt at 160 m above the ellipsoid is about 112 m
    // above sea level.
    let h = height_above_sea_level(50.1109, 8.6821, 160.0);
    assert!((h - (160.0 - undulation(50.1109, 8.6821))).abs() < 1e-12);
    assert!((h - 111.93).abs() < 1.5, "{h}");
    // In the Indian Ocean low the sea lies 107 m below the ellipsoid.
    assert!((height_above_sea_level(4.7, 78.8, 0.0) - 106.9).abs() < 1.5);
}

#[test]
fn integer_variants_agree_with_the_float_ones() {
    for (_, lat, lon, _, _) in REFERENCE {
        let lat_e6 = (lat * 1e6).round() as i32;
        let lon_e6 = (lon * 1e6).round() as i32;
        let n_cm = undulation_cm(lat_e6, lon_e6);
        let n = undulation(f64::from(lat_e6) / 1e6, f64::from(lon_e6) / 1e6);
        assert_eq!(n_cm, (n * 100.0).round() as i32, "{lat} {lon}");
        assert_eq!(
            height_above_sea_level_cm(lat_e6, lon_e6, 16_000),
            16_000 - n_cm
        );
    }
    // Frankfurt: 48.07 m in the 1' model, 48.14 m here.
    assert_eq!(undulation_cm(50_110_900, 8_682_100), 4814);
    assert_eq!(
        height_above_sea_level_cm(50_110_900, 8_682_100, 16_000),
        11_186
    );
    // The extremes of i32 saturate, never wrap or panic.
    assert_eq!(
        height_above_sea_level_cm(4_700_000, 78_800_000, i32::MAX),
        i32::MAX
    );
    assert_eq!(
        height_above_sea_level_cm(-8_400_000, 147_400_000, i32::MIN),
        i32::MIN
    );
    // Extreme microdegrees still read the table.
    let _ = undulation_cm(i32::MAX, i32::MIN);
}

#[test]
fn a_malformed_table_is_refused_and_bad_input_falls_back() {
    assert!(Grid::new(&[]).is_none());
    assert!(Grid::new(&EGM2008_15[..19]).is_none());
    assert!(Grid::new(&EGM2008_15[..EGM2008_15.len() - 1]).is_none());
    let mut wrong_magic = EGM2008_15.to_vec();
    wrong_magic[0] = b'X';
    assert!(Grid::new(&wrong_magic).is_none());
    // Header says 720 rows, which no longer reach pole to pole.
    let mut short = EGM2008_15.to_vec();
    short[4..6].copy_from_slice(&720u16.to_le_bytes());
    assert!(Grid::new(&short).is_none());
    // A zero step.
    let mut zero = EGM2008_15.to_vec();
    zero[8..10].copy_from_slice(&0u16.to_le_bytes());
    assert!(Grid::new(&zero).is_none());

    let grid = Grid::new(EGM2008_15).expect("table reads");
    assert_eq!(grid.undulation(f64::NAN, 0.0), None);
    assert_eq!(grid.undulation(0.0, f64::INFINITY), None);
    assert_eq!(undulation(f64::NAN, 0.0), 0.0);
    assert_eq!(height_above_sea_level(f64::NAN, 0.0, 123.0), 123.0);
}
