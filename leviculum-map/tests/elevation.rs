//! Height pieces (order 1740): the round trip of the encoding on a
//! synthetic ramp, the derived zooms as means of the stored heights, the
//! committed Erg Chebbi cut (zstd frames from the C library, read by the
//! pure-Rust decoder), and every refusal the format names.

use leviculum_map::elevation::{
    heights_to_planes, planes_to_heights, split, write_block, write_directory, Block, Directory,
    DirectoryInfo, ElevationError, TileDecoder, TileFault, TILE_BYTES, TILE_CELLS, TILE_EDGE,
};
use ruzstd::encoding::{compress_to_vec, CompressionLevel};

const CHEBBI: &[u8] = include_bytes!("../fixtures/erg-chebbi.lmh");

fn frame(heights: &[i16]) -> Vec<u8> {
    let mut planes = vec![0u8; TILE_BYTES];
    heights_to_planes(heights, &mut planes);
    compress_to_vec(&planes[..], CompressionLevel::Fastest)
}

/// A ramp over a whole block's grid, rising east and south, so every
/// stored tile differs and every derived mean is known in closed form.
fn ramp(gx: u32, gy: u32) -> i16 {
    (i64::from(gx) * 3 + i64::from(gy) * 5 - 20_000) as i16
}

fn ramp_tile(tx: u32, ty: u32) -> Vec<i16> {
    let mut out = vec![0i16; TILE_CELLS];
    for r in 0..TILE_EDGE as u32 {
        for c in 0..TILE_EDGE as u32 {
            out[(r * TILE_EDGE as u32 + c) as usize] =
                ramp(tx * TILE_EDGE as u32 + c, ty * TILE_EDGE as u32 + r);
        }
    }
    out
}

/// A height file of one z9 block of ramp tiles at (x, y).
fn ramp_file(x: u32, y: u32) -> Vec<u8> {
    let side = 8u32;
    let frames: Vec<Vec<u8>> = (0..side * side)
        .map(|i| frame(&ramp_tile(i % side, i / side)))
        .collect();
    let block = write_block(12, 9, x, y, &frames).expect("write the block");
    let mut file = write_directory(
        &DirectoryInfo {
            tile_zoom: 12,
            block_zoom: 9,
            bounds: [0, 0, 1, 1],
            attribution: "test ramp",
            notice: "no licence: synthetic",
        },
        &[(x, y)],
    )
    .expect("write the directory");
    file.extend_from_slice(&block);
    file
}

#[test]
fn planes_round_trip_every_i16_including_the_wrapping_extremes() {
    let mut heights = vec![0i16; TILE_CELLS];
    for (i, h) in heights.iter_mut().enumerate() {
        *h = match i % 7 {
            0 => i16::MIN,
            1 => i16::MAX,
            2 => -1,
            _ => (i as i32 * 7919 % 65536 - 32768) as i16,
        };
    }
    let mut planes = vec![0u8; TILE_BYTES];
    heights_to_planes(&heights, &mut planes);
    let mut back = vec![0i16; TILE_CELLS];
    planes_to_heights(&planes, &mut back);
    assert_eq!(back, heights);
}

#[test]
fn a_ramp_block_round_trips_and_its_coarser_zooms_are_the_means() {
    let (bx, by) = (300u32, 200u32);
    let file = ramp_file(bx, by);
    let (directory, ranges) = split(&file).expect("split the file");
    assert_eq!(ranges.len(), 2, "the directory and one block");
    assert_eq!(directory.block(0), Some((bx, by)));
    assert_eq!(directory.find(bx, by), Some(0));
    assert_eq!(directory.attribution(), "test ramp");
    let block = Block::parse(&file[ranges[1].clone()]).expect("parse the block");
    assert_eq!(block.side(), 8);

    let mut decoder = TileDecoder::new();
    let mut out = vec![0i16; TILE_CELLS];
    let mut scratch = vec![0i16; TILE_CELLS];
    // Every stored tile, exactly.
    for ty in 0..8 {
        for tx in 0..8 {
            block
                .heights(
                    &mut decoder,
                    12,
                    bx * 8 + tx,
                    by * 8 + ty,
                    &mut out,
                    &mut scratch,
                )
                .expect("decode a stored tile");
            assert_eq!(out, ramp_tile(tx, ty), "z12 tile {tx},{ty}");
        }
    }
    // z11 to z9: each height the mean of the 2^d x 2^d stored heights under
    // it, computed here from the ramp itself, not from the reader.
    for zoom in 9..12u8 {
        let d = 12 - zoom;
        let per = 1u32 << (zoom - 9);
        let cell = 1u32 << d;
        for ty in 0..per {
            for tx in 0..per {
                block
                    .heights(
                        &mut decoder,
                        zoom,
                        bx * per + tx,
                        by * per + ty,
                        &mut out,
                        &mut scratch,
                    )
                    .expect("derive a tile");
                for r in (0..TILE_EDGE as u32).step_by(37) {
                    for c in (0..TILE_EDGE as u32).step_by(41) {
                        let gx0 = (tx * TILE_EDGE as u32 + c) * cell;
                        let gy0 = (ty * TILE_EDGE as u32 + r) * cell;
                        let mut sum = 0i64;
                        for dy in 0..cell {
                            for dx in 0..cell {
                                sum += i64::from(ramp(gx0 + dx, gy0 + dy));
                            }
                        }
                        let n = i64::from(cell * cell);
                        let want = (2 * sum + n).div_euclid(2 * n) as i16;
                        assert_eq!(
                            out[(r * TILE_EDGE as u32 + c) as usize],
                            want,
                            "z{zoom} tile {tx},{ty} at {c},{r}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_erg_chebbi_cut_reads_and_its_z11_is_the_mean_of_its_z12() {
    let (directory, ranges) = split(CHEBBI).expect("split the fixture");
    assert_eq!((directory.block_zoom(), directory.tile_zoom()), (11, 12));
    assert_eq!(directory.block(0), Some((1001, 837)));
    assert!(directory
        .attribution()
        .starts_with("produced using Copernicus WorldDEM-30"));
    assert!(directory.notice().contains("do not incur any liability"));
    let block = Block::parse(&CHEBBI[ranges[1].clone()]).expect("parse the block");

    let mut decoder = TileDecoder::new();
    let mut scratch = vec![0i16; TILE_CELLS];
    let mut stored: Vec<Vec<i16>> = Vec::new();
    for i in 0..4u32 {
        let mut out = vec![0i16; TILE_CELLS];
        block
            .heights(
                &mut decoder,
                12,
                2002 + i % 2,
                1674 + i / 2,
                &mut out,
                &mut scratch,
            )
            .expect("decode a stored tile");
        // The dunes and the plain they sit on: the cut said 692 to 904 m.
        let (min, max) = (out.iter().min().copied(), out.iter().max().copied());
        assert!(
            min >= Some(650) && max <= Some(950),
            "z12 tile {i}: {min:?}..{max:?}"
        );
        stored.push(out);
    }
    // Dunes, not a plane: the heights spread over more than 100 m.
    let all: Vec<i16> = stored.iter().flatten().copied().collect();
    let spread = all.iter().max().copied().unwrap_or(0) - all.iter().min().copied().unwrap_or(0);
    assert!(spread > 100, "spread {spread} m");

    let mut z11 = vec![0i16; TILE_CELLS];
    block
        .heights(&mut decoder, 11, 1001, 837, &mut z11, &mut scratch)
        .expect("derive z11");
    for r in 0..TILE_EDGE {
        for c in 0..TILE_EDGE {
            let tile = &stored[(r / 128) * 2 + c / 128];
            let (sr, sc) = ((r % 128) * 2, (c % 128) * 2);
            let sum: i32 = [(0, 0), (0, 1), (1, 0), (1, 1)]
                .iter()
                .map(|(dy, dx)| i32::from(tile[(sr + dy) * TILE_EDGE + sc + dx]))
                .sum();
            assert_eq!(
                z11[r * TILE_EDGE + c],
                (2 * sum + 4).div_euclid(8) as i16,
                "z11 at {c},{r}"
            );
        }
    }
}

#[test]
fn a_corrupt_piece_is_refused() {
    let (_, ranges) = split(CHEBBI).expect("split the fixture");
    let block_bytes = &CHEBBI[ranges[1].clone()];
    let mut decoder = TileDecoder::new();
    let mut out = vec![0i16; TILE_CELLS];
    let mut scratch = vec![0i16; TILE_CELLS];

    // A flipped byte inside the first tile's compressed data: the frame
    // breaks or its checksum disagrees, never wrong heights.
    let tile0 = 24 + 4 * 4;
    for offset in [tile0 + 40, tile0 + 2000, tile0 + 9000] {
        let mut bad = block_bytes.to_vec();
        bad[offset] ^= 0x5a;
        let block = Block::parse(&bad).expect("the header is intact");
        let got = block.heights(&mut decoder, 12, 2002, 1674, &mut out, &mut scratch);
        assert!(
            matches!(
                got,
                Err(ElevationError::Tile {
                    index: 0,
                    fault: TileFault::Frame | TileFault::Checksum | TileFault::Size
                })
            ),
            "flip at {offset}: {got:?}"
        );
    }

    // The header's own fields.
    let mut bad = block_bytes.to_vec();
    bad[0] = b'X';
    assert_eq!(Block::parse(&bad).err(), Some(ElevationError::Magic));
    let mut bad = block_bytes.to_vec();
    bad[4] = 2;
    assert_eq!(Block::parse(&bad).err(), Some(ElevationError::Version(2)));
    let mut bad = block_bytes.to_vec();
    bad[6] = 28;
    assert_eq!(Block::parse(&bad).err(), Some(ElevationError::HeaderLength));
    let mut bad = block_bytes.to_vec();
    bad[11] = 0; // tile edge 0 (256 is 00 01 little-endian)
    assert_eq!(Block::parse(&bad).err(), Some(ElevationError::TileEdge));
    let mut bad = block_bytes.to_vec();
    bad[9] = 7; // block zoom five above the tile zoom
    assert_eq!(Block::parse(&bad).err(), Some(ElevationError::Zooms));
    let mut bad = block_bytes.to_vec();
    bad[20] = 5; // tile count 5 for a 2 x 2 block
    assert_eq!(Block::parse(&bad).err(), Some(ElevationError::TileCount));
    let mut bad = block_bytes.to_vec();
    bad[12..16].copy_from_slice(&5000u32.to_le_bytes()); // x past z11's 2048
    assert_eq!(Block::parse(&bad).err(), Some(ElevationError::Coordinates));
    // Truncated, and with a byte too many.
    assert_eq!(
        Block::parse(&block_bytes[..block_bytes.len() - 1]).err(),
        Some(ElevationError::Length)
    );
    let mut long = block_bytes.to_vec();
    long.push(0);
    assert_eq!(Block::parse(&long).err(), Some(ElevationError::Length));

    // The directory and the file.
    let directory = &CHEBBI[ranges[0].clone()];
    let mut bad = directory.to_vec();
    bad[4] = 9;
    assert_eq!(
        Directory::parse(&bad).err(),
        Some(ElevationError::Version(9))
    );
    let mut bad = directory.to_vec();
    bad[40] = 0xff; // the attribution's first byte, not UTF-8
    assert_eq!(Directory::parse(&bad).err(), Some(ElevationError::Text));
    // A block the directory does not name at its place.
    let mut bad = CHEBBI.to_vec();
    let at = ranges[1].start + 16;
    bad[at..at + 4].copy_from_slice(&838u32.to_le_bytes());
    assert_eq!(split(&bad).err(), Some(ElevationError::Coordinates));

    // A height asked for outside the block, or at a zoom it does not span.
    let block = Block::parse(block_bytes).expect("parse");
    assert_eq!(
        block.heights(&mut decoder, 12, 2004, 1674, &mut out, &mut scratch),
        Err(ElevationError::Outside)
    );
    assert_eq!(
        block.heights(&mut decoder, 10, 500, 418, &mut out, &mut scratch),
        Err(ElevationError::Outside)
    );
}

#[test]
fn a_frame_without_its_checksum_is_refused() {
    // ruzstd writes the checksum with its `hash` feature; strip the flag
    // and the four trailing bytes to build the frame a careless writer
    // would.
    let heights = ramp_tile(0, 0);
    let mut f = frame(&heights);
    // Frame header descriptor: byte 4, bit 2 is the checksum flag.
    assert_ne!(f[4] & 0x04, 0, "the encoder wrote a checksum");
    f[4] &= !0x04;
    f.truncate(f.len() - 4);
    let mut out = vec![0i16; TILE_CELLS];
    assert_eq!(
        TileDecoder::new().decode(&f, &mut out),
        Err(TileFault::NoChecksum)
    );
}
