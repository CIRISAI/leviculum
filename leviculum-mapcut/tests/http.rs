//! The archive over HTTP range requests (1089), against the loopback
//! server and the committed fixture, never the network: the same tiles as
//! from the file, few requests, the size cap lowering the deepest zoom, a
//! failing and an unreachable source told apart, and the daily build found
//! by date.
#![cfg(feature = "http")]

use std::io::Cursor;

use leviculum_map::region::{Bounds, Region};
use leviculum_mapcut::http::{daily, daily_url, unreachable, HttpRange};
use leviculum_mapcut::load::{deepest_within, load, Empty, TooLarge, Want};
use leviculum_mapcut::pmtiles::Archive;
use leviculum_mapcut::serve::Serve;

const FIXTURE: &[u8] = include_bytes!("../fixtures/oltrarno.pmtiles");

/// The fixture's box (`fixtures/README.md`).
const BOX: Bounds = Bounds {
    west: 112_615_000,
    south: 437_405_000,
    east: 112_825_000,
    north: 437_545_000,
};

fn served() -> Serve {
    Serve::start(vec![("oltrarno.pmtiles".to_owned(), FIXTURE.to_vec())]).expect("serve")
}

fn want(cap: u64) -> Want {
    Want {
        bounds: BOX,
        zooms: 10..=15,
        floor: 14,
        cap,
        profile: leviculum_map::profile::CUT_1,
        locale: Some("de".to_owned()),
        buildings: true,
        pois: true,
    }
}

#[test]
fn the_remote_archive_reads_the_same_tiles_as_the_file_in_few_requests() {
    let serve = served();
    let remote =
        Archive::from_source(HttpRange::new(&serve.url("oltrarno.pmtiles"))).expect("open");
    let opened = serve.requests();
    assert_eq!(opened, 1, "the header and the root in one read");
    let path = std::env::temp_dir().join(format!("lmc-http-{}.pmtiles", std::process::id()));
    std::fs::write(&path, FIXTURE).expect("write");
    let local = Archive::open(&path).expect("open the file");
    let _ = std::fs::remove_file(&path);
    let there = remote.locate_in(&BOX, 0..=15).expect("locate");
    assert_eq!(there, local.locate_in(&BOX, 0..=15).expect("locate"));
    assert_eq!(there.len(), 6, "{there:?}");
    let fetched = remote.fetch(&there).expect("fetch");
    assert!(fetched.requests <= 2, "{} ranges", fetched.requests);
    assert_eq!(serve.requests(), opened + fetched.requests);
    for (t, ..) in &there {
        assert_eq!(fetched.mvt(*t).expect("mvt"), local.mvt(*t).expect("mvt"));
    }
}

#[test]
fn a_load_cuts_the_box_into_a_region_and_lowers_the_deepest_zoom_to_the_cap() {
    let serve = served();
    let remote =
        Archive::from_source(HttpRange::new(&serve.url("oltrarno.pmtiles"))).expect("open");
    let mut out = Cursor::new(Vec::new());
    let loaded = load(&remote, &want(1 << 20), "loquax", 2, &mut out).expect("load");
    assert_eq!(loaded.zooms, 10..=15);
    let bytes = out.into_inner();
    let region = Region::open(bytes.as_slice()).expect("a region");
    region.verify().expect("whole");
    assert_eq!(region.header().bounds, BOX);
    assert!(region
        .notice()
        .contains("Cut by loquax under layer profile 1"));
    // Under a cap that the z15 tiles do not fit, the region ends at z14.
    let z14: u64 = remote
        .locate_in(&BOX, 0..=14)
        .expect("locate")
        .iter()
        .map(|l| u64::from(l.2))
        .sum();
    let mut out = Cursor::new(Vec::new());
    let loaded = load(&remote, &want(z14), "loquax", 2, &mut out).expect("load");
    assert_eq!(loaded.zooms, 10..=14);
    let bytes = out.into_inner();
    let region = Region::open(bytes.as_slice()).expect("region");
    assert_eq!(region.header().max_zoom, 14);
    // Below that the floor holds, and the load says how large it is.
    let error = load(
        &remote,
        &want(z14 - 1),
        "loquax",
        2,
        Cursor::new(Vec::new()),
    )
    .expect_err("over the cap");
    let too = error.downcast_ref::<TooLarge>().expect("TooLarge");
    assert_eq!(
        *too,
        TooLarge {
            bytes: z14,
            cap: z14 - 1
        }
    );
    // A box where the archive has nothing is not a region.
    let mut far = want(1 << 20);
    far.bounds = Bounds {
        west: 80_000_000,
        south: 470_000_000,
        east: 80_100_000,
        north: 470_100_000,
    };
    let error = load(&remote, &far, "loquax", 2, Cursor::new(Vec::new())).expect_err("empty");
    assert!(error.downcast_ref::<Empty>().is_some(), "{error}");
}

#[test]
fn the_deepest_zoom_within_the_cap() {
    let tiles = [(10, 100), (11, 100), (12, 400), (13, 1600)];
    assert_eq!(
        deepest_within(&tiles, &(10..=13), 10, 10_000),
        Ok((13, 2200))
    );
    assert_eq!(deepest_within(&tiles, &(10..=13), 10, 600), Ok((12, 600)));
    assert_eq!(deepest_within(&tiles, &(10..=13), 12, 599), Err(600));
    assert_eq!(deepest_within(&tiles, &(10..=13), 10, 99), Err(100));
}

#[test]
fn a_failing_source_and_an_unreachable_one_are_told_apart() {
    let serve = served();
    serve.fail(true);
    let error = Archive::from_source(HttpRange::new(&serve.url("oltrarno.pmtiles")))
        .err()
        .expect("503");
    assert!(!unreachable(error.as_ref()), "{error}");
    assert!(error.to_string().contains("503"), "{error}");
    serve.fail(false);
    let missing = Archive::from_source(HttpRange::new(&serve.url("nothing.pmtiles")))
        .err()
        .expect("404");
    assert!(missing.to_string().contains("404"), "{missing}");
    let base = serve.base().to_owned();
    drop(serve);
    let error = Archive::from_source(HttpRange::new(&format!("{base}oltrarno.pmtiles")))
        .err()
        .expect("nothing listens");
    assert!(unreachable(error.as_ref()), "{error}");
}

#[test]
fn the_newest_daily_build_that_answers_is_found() {
    // 2026-10-28 12:00 UTC; the server holds the build of the 26th only.
    let now = 1_793_188_800;
    let today = now / 86_400;
    assert_eq!(daily_url("x/", today), "x/20261028.pmtiles");
    let serve =
        Serve::start(vec![("20261026.pmtiles".to_owned(), FIXTURE.to_vec())]).expect("serve");
    let found = daily(serve.base(), now).expect("found");
    assert_eq!(found.url(), serve.url("20261026.pmtiles"));
    assert_eq!(serve.requests(), 3, "the 28th, the 27th, the 26th");
    let empty = Serve::start(Vec::new()).expect("serve");
    assert!(daily(empty.base(), now).is_err());
}
