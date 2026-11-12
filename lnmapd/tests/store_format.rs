//! The store format over a real region: the split is a prefix ladder
//! (every piece boundary leaves a readable region, coarse zooms first),
//! the pieces reassemble byte-identically, and the signed index is
//! verified against the pinned destination — wrong pin, tampered
//! bytes, tampered piece and generation rollback all refused.

mod common;

use leviculum_core::crypto::sha256;
use leviculum_map::region::{Region, Slot};
use leviculum_std::generate_identity;
use lnmapd::store::{
    add_pack, index_path, piece_path, verify_piece, AddOptions, Index, VerifyError, KIND_VECTORS,
};

fn options(id: &str) -> AddOptions {
    AddOptions {
        id: id.to_owned(),
        kind: KIND_VECTORS.to_owned(),
        http_url: Some("http://127.0.0.1:0/maps".to_owned()),
        replace: false,
    }
}

#[test]
fn the_split_is_a_prefix_ladder_and_reassembles() {
    let region = common::grunewald_region();
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();

    let written =
        add_pack(store.path(), &region, &identity, &options("grunewald")).expect("add the pack");

    // The index on disk verifies against the pin a device holds: the
    // identity's own lev.maps destination.
    let pinned = Index::destination_of(&identity.public_key_bytes()).expect("own destination");
    let bytes = std::fs::read(index_path(store.path())).expect("read index");
    let index = Index::verify(&bytes, &pinned, None).expect("verify the index");
    assert_eq!(index, written, "the returned and the stored index agree");
    assert_eq!(index.generation, 1);

    let pack = index.pack("grunewald").expect("the pack is in the index");
    assert_eq!((pack.min_zoom, pack.max_zoom), (12, 15));
    assert_eq!(pack.total, region.len() as u64);
    assert_eq!(pack.whole, sha256(&region));
    assert!(
        pack.pieces.len() >= 2,
        "four zooms must split into more than one piece, got {}",
        pack.pieces.len()
    );

    // Reassemble from the piece files on disk: byte-identical, and
    // every prefix boundary is a readable region whose tiles up to the
    // piece's zoom are all there — fetching pieces in order IS coarse
    // before fine.
    let mut assembled: Vec<u8> = Vec::new();
    for piece in &pack.pieces {
        let bytes = std::fs::read(piece_path(store.path(), &piece.id())).expect("read piece");
        verify_piece(piece, &bytes).expect("piece verifies");
        assert_eq!(piece.offset, assembled.len() as u64, "contiguous pieces");
        assembled.extend_from_slice(&bytes);

        let prefix = Region::from_bytes(&assembled).expect("prefix opens as a region");
        for n in 0..prefix.header().tiles {
            let (id, slot) = prefix.slot(n).expect("slot reads");
            if id.z <= piece.zoom {
                assert!(
                    matches!(slot, Slot::Here(_) | Slot::Empty),
                    "tile z{} {}/{} must be there once the z{} piece arrived",
                    id.z,
                    id.x,
                    id.y,
                    piece.zoom
                );
            }
        }
    }
    assert_eq!(assembled, region, "the pieces reassemble the exact file");
    assert_eq!(
        pack.pieces.last().map(|p| p.zoom),
        Some(15),
        "the last piece completes the deepest zoom"
    );

    // The attribution piece carries the region's own texts.
    let attribution = std::fs::read(piece_path(store.path(), &hex::encode(pack.attribution)))
        .expect("read attribution piece");
    let text = String::from_utf8(attribution).expect("attribution is UTF-8");
    assert!(text.contains("OpenStreetMap"), "attribution line: {text}");
    assert!(text.contains("ODbL"), "notice line: {text}");
    assert_eq!(text.len() as u32, pack.attribution_len);
}

#[test]
fn the_wrong_pin_and_tampered_bytes_are_refused() {
    let region = common::grunewald_region();
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();
    add_pack(store.path(), &region, &identity, &options("grunewald")).expect("add the pack");
    let bytes = std::fs::read(index_path(store.path())).expect("read index");

    // A different server's destination: refused by name.
    let other = generate_identity();
    let wrong_pin = Index::destination_of(&other.public_key_bytes()).expect("other destination");
    assert!(
        matches!(
            Index::verify(&bytes, &wrong_pin, None),
            Err(VerifyError::NotThePinnedServer { .. })
        ),
        "an index signed by another key must not verify against the pin"
    );

    // One byte flipped in the body: the signature refuses.
    let pinned = Index::destination_of(&identity.public_key_bytes()).expect("own destination");
    let mut tampered = bytes.clone();
    tampered[40] ^= 1;
    assert_eq!(
        Index::verify(&tampered, &pinned, None),
        Err(VerifyError::Signature)
    );

    // One byte flipped in the signature itself: refused too.
    let mut tampered = bytes.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert_eq!(
        Index::verify(&tampered, &pinned, None),
        Err(VerifyError::Signature)
    );

    // A generation behind the one already seen: a rollback, refused
    // even though the signature is fine.
    let index = Index::verify(&bytes, &pinned, None).expect("the untouched index verifies");
    assert_eq!(
        Index::verify(&bytes, &pinned, Some(index.generation + 1)),
        Err(VerifyError::Rollback {
            got: index.generation,
            floor: index.generation + 1
        })
    );
}

#[test]
fn a_tampered_piece_is_refused() {
    let region = common::grunewald_region();
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();
    let index =
        add_pack(store.path(), &region, &identity, &options("grunewald")).expect("add the pack");
    let piece = &index.pack("grunewald").expect("pack").pieces[0];
    let mut bytes = std::fs::read(piece_path(store.path(), &piece.id())).expect("read piece");

    verify_piece(piece, &bytes).expect("the untouched piece verifies");
    bytes[0] ^= 1;
    assert!(
        matches!(
            verify_piece(piece, &bytes),
            Err(VerifyError::Tampered { reason: "hash", .. })
        ),
        "a flipped byte must fail the hash"
    );
    bytes.pop();
    assert!(
        matches!(
            verify_piece(piece, &bytes),
            Err(VerifyError::Tampered {
                reason: "length",
                ..
            })
        ),
        "a short piece must fail the length before hashing"
    );
}

#[test]
fn adding_again_replaces_the_pack_and_grows_the_generation() {
    let region = common::grunewald_region();
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();
    add_pack(store.path(), &region, &identity, &options("grunewald")).expect("first add");
    let second =
        add_pack(store.path(), &region, &identity, &options("grunewald")).expect("second add");

    assert_eq!(second.generation, 2, "every write grows the generation");
    assert_eq!(second.packs.len(), 1, "same id replaces, never duplicates");
    assert_eq!(
        second.http_url, "http://127.0.0.1:0/maps",
        "the index names the server's HTTP address"
    );
}
