//! The PMTiles reader on a small archive written here: Hilbert ids, the
//! varint directory, runs and a leaf directory.

use leviculum_map::region::TileId;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use leviculum_mapcut::pmtiles::{parse_directory, tile_id, tile_of, write, Archive, RangeRead};
use leviculum_mapcut::Res;

#[test]
fn hilbert_ids_round_trip() {
    for z in 0..=7u8 {
        for x in 0..(1u32 << z) {
            for y in 0..(1u32 << z) {
                let t = TileId { z, x, y };
                assert_eq!(tile_of(tile_id(t)), Some(t));
            }
        }
    }
    // The spec's order at zoom 1, and the first id of zoom 2.
    let ids: Vec<u64> = [(0, 0), (0, 1), (1, 1), (1, 0)]
        .iter()
        .map(|&(x, y)| tile_id(TileId { z: 1, x, y }))
        .collect();
    assert_eq!(ids, [1, 2, 3, 4]);
    assert_eq!(tile_id(TileId { z: 2, x: 0, y: 0 }), 5);
}

fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// (tile id, offset, length, run length), offsets written explicitly.
fn directory(entries: &[(u64, u64, u32, u32)]) -> Vec<u8> {
    let mut b = Vec::new();
    varint(entries.len() as u64, &mut b);
    let mut last = 0;
    for e in entries {
        varint(e.0 - last, &mut b);
        last = e.0;
    }
    for e in entries {
        varint(u64::from(e.3), &mut b);
    }
    for e in entries {
        varint(u64::from(e.2), &mut b);
    }
    for (i, e) in entries.iter().enumerate() {
        let contiguous = i > 0 && entries[i - 1].1 + u64::from(entries[i - 1].2) == e.1;
        varint(if contiguous { 0 } else { e.1 + 1 }, &mut b);
    }
    b
}

#[test]
fn an_archive_with_a_run_and_a_leaf() {
    // Tiles: "A" at z0; "BB" for z1 ids 1 and 2 as one run; a leaf with
    // "C" at z2 (0, 0).
    let data = b"ABBC".to_vec();
    let z2 = tile_id(TileId { z: 2, x: 0, y: 0 });
    let leaf = directory(&[(z2, 3, 1, 1)]);
    let root = directory(&[(0, 0, 1, 1), (1, 1, 2, 2), (z2, 0, leaf.len() as u32, 0)]);
    assert_eq!(parse_directory(&root).expect("dir").len(), 3);

    let header_len = 127u64;
    let root_at = header_len;
    let meta_at = root_at + root.len() as u64;
    let leaf_at = meta_at;
    let data_at = leaf_at + leaf.len() as u64;
    let mut h = vec![0u8; 127];
    h[..7].copy_from_slice(b"PMTiles");
    h[7] = 3;
    let mut put = |at: usize, v: u64| h[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put(8, root_at);
    put(16, root.len() as u64);
    put(24, meta_at);
    put(32, 0);
    put(40, leaf_at);
    put(48, leaf.len() as u64);
    put(56, data_at);
    put(64, data.len() as u64);
    h[97] = 1; // directories uncompressed
    h[98] = 1; // tiles uncompressed
    h[99] = 1; // MVT
    h[101] = 2;
    let mut file = h;
    file.extend_from_slice(&root);
    file.extend_from_slice(&leaf);
    file.extend_from_slice(&data);
    let path = std::env::temp_dir().join(format!("lmt-pmtiles-{}.pmtiles", std::process::id()));
    std::fs::write(&path, &file).expect("write");
    let a = Archive::open(&path).expect("open");
    let tiles: Vec<TileId> = a.tiles().expect("walk");
    let _ = std::fs::remove_file(&path);
    let t = |z, x, y| TileId { z, x, y };
    assert_eq!(tiles, [t(0, 0, 0), t(1, 0, 0), t(1, 0, 1), t(2, 0, 0)]);
    assert_eq!(a.mvt(t(0, 0, 0)).expect("read"), Some(b"A".to_vec()));
    assert_eq!(a.mvt(t(1, 0, 1)).expect("read"), Some(b"BB".to_vec()));
    assert_eq!(a.mvt(t(2, 0, 0)).expect("read"), Some(b"C".to_vec()));
    assert_eq!(a.mvt(t(1, 1, 1)).expect("read"), None);
}

#[test]
fn a_directory_cut_short_is_an_error() {
    let root = directory(&[(0, 0, 1, 1), (1, 1, 2, 2)]);
    for cut in 0..root.len() {
        assert!(parse_directory(&root[..cut]).is_err(), "cut at {cut}");
    }
}

/// An archive held in memory, read as a file would be.
struct Held(Vec<u8>);

impl RangeRead for Held {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let start = usize::try_from(offset)?.min(self.0.len());
        let end = usize::try_from(offset.saturating_add(len))?.min(self.0.len());
        Ok(self.0[start..end].to_vec())
    }
}

#[test]
fn a_root_of_every_tile_is_searched_in_place() {
    // An extract writes one root holding every tile's entry, Germany's
    // parts hundreds of thousands each. `locate` once copied that root for
    // every tile it looked up, so the cut of a country did not end in ten
    // minutes (1193). Every z9 tile, 262,144 entries: searched in place it
    // is milliseconds, copied it is terabytes of memcpy.
    let z = 9u8;
    let mut tiles = BTreeMap::new();
    for x in 0..(1u32 << z) {
        for y in 0..(1u32 << z) {
            tiles.insert(TileId { z, x, y }, vec![(x ^ y) as u8]);
        }
    }
    let mut file = Vec::new();
    write(&tiles, 1, &serde_json::json!({}), &mut file).expect("write");
    let a = Archive::from_source(Held(file)).expect("open");
    // Checked as it goes, so the copy fails at the bound and does not hang.
    let bound = Duration::from_secs(20);
    let started = Instant::now();
    for (i, (&t, blob)) in tiles.iter().enumerate() {
        let (at, _, len) = a.locate(t).expect("locate").expect("held");
        assert_eq!(at, t);
        assert_eq!(len as usize, blob.len());
        assert!(
            started.elapsed() < bound,
            "{i} of 262,144 lookups in a root of as many entries took over {bound:?}"
        );
    }
    assert_eq!(
        a.locate(TileId { z: 10, x: 0, y: 0 }).expect("locate"),
        None
    );
}
