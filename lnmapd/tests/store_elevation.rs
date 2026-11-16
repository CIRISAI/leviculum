//! Height packs in the store (order 1740): a height file splits into its
//! directory piece and one piece per block, every piece verifies against
//! the signed index, the pieces reassemble the exact file, each block
//! piece reads on its own, and the attribution piece carries the source's
//! own licence words. `lnmapcut` tells a height file from a region file by
//! its magic.

use std::process::Command;

use leviculum_core::crypto::sha256;
use leviculum_map::elevation::{Block, Directory, TileDecoder, TILE_CELLS};
use leviculum_std::{generate_identity, Identity};
use lnmapd::store::{
    add_pack, index_path, piece_path, verify_piece, AddOptions, Index, KIND_ELEVATION,
};

const CHEBBI: &[u8] = include_bytes!("../../leviculum-map/fixtures/erg-chebbi.lmh");

#[test]
fn a_height_file_splits_into_its_directory_and_standalone_blocks() {
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();
    let options = AddOptions {
        id: "erg-chebbi".to_owned(),
        kind: KIND_ELEVATION.to_owned(),
        http_url: None,
        replace: false,
    };
    add_pack(store.path(), CHEBBI, &identity, &options).expect("add the pack");
    let pinned = Index::destination_of(&identity.public_key_bytes()).expect("destination");
    let bytes = std::fs::read(index_path(store.path())).expect("read index");
    let index = Index::verify(&bytes, &pinned, None).expect("verify");
    let pack = index.pack("erg-chebbi").expect("the pack");
    assert_eq!(pack.kind, KIND_ELEVATION);
    assert_eq!((pack.min_zoom, pack.max_zoom), (11, 12));
    assert_eq!(pack.total, CHEBBI.len() as u64);
    assert_eq!(pack.whole, sha256(CHEBBI));
    assert_eq!(pack.pieces.len(), 2, "the directory and one block");
    assert_eq!(
        pack.pieces.iter().map(|p| p.zoom).collect::<Vec<_>>(),
        vec![0, 12]
    );

    let mut assembled = Vec::new();
    for (n, piece) in pack.pieces.iter().enumerate() {
        let bytes = std::fs::read(piece_path(store.path(), &piece.id())).expect("read piece");
        verify_piece(piece, &bytes).expect("piece verifies");
        assert_eq!(piece.offset, assembled.len() as u64);
        if n == 0 {
            let directory = Directory::parse(&bytes).expect("the directory reads alone");
            assert_eq!(pack.bounds, directory.bounds());
        } else {
            let block = Block::parse(&bytes).expect("the block reads alone");
            let (x, y) = block.position();
            let mut out = vec![0i16; TILE_CELLS];
            let mut scratch = vec![0i16; TILE_CELLS];
            block
                .heights(&mut TileDecoder::new(), 11, x, y, &mut out, &mut scratch)
                .expect("derive z11 from the piece alone");
        }
        assembled.extend_from_slice(&bytes);
    }
    assert_eq!(assembled, CHEBBI);

    let attribution = std::fs::read(piece_path(store.path(), &hex::encode(pack.attribution)))
        .expect("attribution piece");
    let text = String::from_utf8(attribution).expect("utf-8");
    assert!(text.starts_with("produced using Copernicus WorldDEM-30 © DLR e.V."));
    assert!(text.contains("do not incur any liability"));
}

#[test]
fn a_damaged_height_file_is_refused_and_the_store_untouched() {
    let store = tempfile::tempdir().expect("store dir");
    let mut bad = CHEBBI.to_vec();
    bad.pop();
    let options = AddOptions {
        id: "erg-chebbi".to_owned(),
        kind: KIND_ELEVATION.to_owned(),
        http_url: None,
        replace: false,
    };
    assert!(add_pack(store.path(), &bad, &generate_identity(), &options).is_err());
    assert!(!index_path(store.path()).exists());
}

#[test]
fn lnmapcut_adds_a_height_file_as_an_elevation_pack() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("erg-chebbi.lmh");
    std::fs::write(&file, CHEBBI).expect("write");
    let store = dir.path().join("store");
    let identity_path = dir.path().join("identity");
    let output = Command::new(env!("CARGO_BIN_EXE_lnmapcut"))
        .arg("--store")
        .arg(&store)
        .arg("--identity")
        .arg(&identity_path)
        .arg("--id")
        .arg("erg-chebbi")
        .arg(&file)
        .output()
        .expect("run lnmapcut");
    assert!(
        output.status.success(),
        "lnmapcut failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let key = std::fs::read(&identity_path).expect("read identity");
    let identity = Identity::from_private_key_bytes(&key).expect("identity");
    let pinned = Index::destination_of(&identity.public_key_bytes()).expect("destination");
    let index = Index::verify(
        &std::fs::read(index_path(&store)).expect("index"),
        &pinned,
        None,
    )
    .expect("verify");
    assert_eq!(
        index.pack("erg-chebbi").map(|p| p.kind.as_str()),
        Some(KIND_ELEVATION)
    );
}

mod common;

/// The audit of 1740 (order 1758): a height file added under the id of a
/// region pack silently took the region's place in the catalogue, and a
/// device that had the region pinned by id then fetched heights. A pack
/// of another kind under an existing id is refused, the index untouched.
#[test]
fn audit_height_pack_does_not_replace_a_region_pack_of_the_same_id() {
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();
    let region = common::grunewald_region();
    let vectors = AddOptions {
        id: "grunewald".to_owned(),
        kind: lnmapd::store::KIND_VECTORS.to_owned(),
        http_url: None,
        replace: false,
    };
    add_pack(store.path(), &region, &identity, &vectors).expect("add the region");
    let heights = AddOptions {
        id: "grunewald".to_owned(),
        kind: KIND_ELEVATION.to_owned(),
        http_url: None,
        replace: false,
    };
    let refused = add_pack(store.path(), CHEBBI, &identity, &heights);
    let message = refused.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("grunewald") && message.contains("vectors"),
        "the second add must be refused, naming the pack and its kind: {message:?}"
    );

    let pinned = Index::destination_of(&identity.public_key_bytes()).expect("destination");
    let bytes = std::fs::read(index_path(store.path())).expect("read index");
    let index = Index::verify(&bytes, &pinned, None).expect("verify");
    assert_eq!(index.generation, 1, "the index was not rewritten");
    let pack = index.pack("grunewald").expect("the pack");
    assert_eq!(pack.kind, lnmapd::store::KIND_VECTORS);
    assert_eq!(pack.whole, sha256(&region));
}

/// lnmapcut prints the refusal and exits non-zero.
#[test]
fn lnmapcut_refuses_a_height_file_under_a_region_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let region = dir.path().join("grunewald.lmr");
    std::fs::write(&region, common::grunewald_region()).expect("write region");
    let heights = dir.path().join("erg-chebbi.lmh");
    std::fs::write(&heights, CHEBBI).expect("write heights");
    let store = dir.path().join("store");
    let identity_path = dir.path().join("identity");
    let run = |file: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_lnmapcut"))
            .arg("--store")
            .arg(&store)
            .arg("--identity")
            .arg(&identity_path)
            .arg("--id")
            .arg("grunewald")
            .arg(file)
            .output()
            .expect("run lnmapcut")
    };
    let first = run(&region);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run(&heights);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        !second.status.success(),
        "lnmapcut accepted the height file"
    );
    assert!(
        stderr.contains("grunewald") && stderr.contains("vectors"),
        "the refusal is printed: {stderr}"
    );
}

/// `fetch_prefix` walks a prefix ladder; a height pack is not one (its
/// blocks stand alone and its piece zoom byte is not "deepest zoom
/// complete"), so the planner refuses it instead of reporting a zoom.
#[test]
fn fetch_prefix_refuses_an_elevation_pack() {
    struct FromStore(std::path::PathBuf);
    impl lnmapd::client::PieceFetcher for FromStore {
        fn fetch_piece(&mut self, id: &str) -> Result<Vec<u8>, lnmapd::client::FetchError> {
            std::fs::read(piece_path(&self.0, id)).map_err(|e| {
                lnmapd::client::FetchError::Transport {
                    id: id.to_owned(),
                    reason: e.to_string(),
                }
            })
        }
    }
    struct Never;
    impl lnmapd::client::Clock for Never {
        fn elapsed(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
    }
    let store = tempfile::tempdir().expect("store dir");
    let options = AddOptions {
        id: "erg-chebbi".to_owned(),
        kind: KIND_ELEVATION.to_owned(),
        http_url: None,
        replace: false,
    };
    let index =
        add_pack(store.path(), CHEBBI, &generate_identity(), &options).expect("add the pack");
    let pack = index.pack("erg-chebbi").expect("the pack");
    let mut out = Vec::new();
    let got = lnmapd::client::fetch_prefix(
        &mut FromStore(store.path().to_owned()),
        &Never,
        pack,
        std::time::Duration::from_secs(60),
        &mut out,
    );
    let message = match got {
        Ok(fetched) => panic!("an elevation pack was fetched as a prefix ladder: {fetched:?}"),
        Err(e) => e.to_string(),
    };
    assert!(message.contains("elevation"), "{message}");
    assert!(out.is_empty(), "nothing was assembled");
}

/// With the explicit replace option the height pack takes the region's
/// id, and the next generation holds the elevation pack.
#[test]
fn an_explicit_replace_lets_another_kind_take_the_id() {
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();
    let vectors = AddOptions {
        id: "grunewald".to_owned(),
        kind: lnmapd::store::KIND_VECTORS.to_owned(),
        http_url: None,
        replace: false,
    };
    add_pack(
        store.path(),
        &common::grunewald_region(),
        &identity,
        &vectors,
    )
    .expect("add the region");
    let heights = AddOptions {
        id: "grunewald".to_owned(),
        kind: KIND_ELEVATION.to_owned(),
        http_url: None,
        replace: true,
    };
    let index = add_pack(store.path(), CHEBBI, &identity, &heights).expect("replace");
    assert_eq!(index.generation, 2);
    assert_eq!(
        index.pack("grunewald").map(|p| p.kind.as_str()),
        Some(KIND_ELEVATION)
    );
}
