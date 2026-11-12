//! The cutter binary end to end: `lnmapcut` mints its identity, writes
//! the store, and a second run replaces the pack under a grown
//! generation — driven through the real command line.

mod common;

use std::process::Command;

use leviculum_std::Identity;
use lnmapd::store::{index_path, Index};

#[test]
fn lnmapcut_writes_a_store_the_identity_pin_verifies() {
    let dir = tempfile::tempdir().expect("temp dir");
    let region_path = dir.path().join("grunewald.lmr");
    std::fs::write(&region_path, common::grunewald_region()).expect("write region");
    let store = dir.path().join("store");
    let identity_path = dir.path().join("identity");

    let run = |expect: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_lnmapcut"))
            .arg("--store")
            .arg(&store)
            .arg("--identity")
            .arg(&identity_path)
            .arg("--id")
            .arg("grunewald")
            .arg("--http-url")
            .arg("http://127.0.0.1:0/maps")
            .arg(&region_path)
            .output()
            .expect("run lnmapcut");
        assert!(
            output.status.success(),
            "lnmapcut failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(stdout.contains(expect), "stdout: {stdout}");
    };

    run("generation 1");
    run("generation 2");

    // The identity the first run minted signs an index that verifies
    // against that identity's lev.maps destination — the pin a device
    // would hold.
    let key = std::fs::read(&identity_path).expect("read identity");
    let identity = Identity::from_private_key_bytes(&key).expect("parse identity");
    let pinned = Index::destination_of(&identity.public_key_bytes()).expect("destination");
    let bytes = std::fs::read(index_path(&store)).expect("read index");
    let index = Index::verify(&bytes, &pinned, None).expect("verify");
    assert_eq!(index.generation, 2);
    assert_eq!(index.packs.len(), 1);
    assert_eq!(index.http_url, "http://127.0.0.1:0/maps");
}
