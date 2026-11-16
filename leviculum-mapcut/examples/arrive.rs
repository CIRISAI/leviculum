//! A region arriving over a slow link (1291): an archive served from
//! memory by [`Serve`] at a throttled rate, loaded as the autoload loads,
//! a round at a time into a version 4 file, and the file opened after
//! every round as a painter opens it. Prints when the first tile could be
//! drawn, when every tile of a zoom had arrived, and the total.
//!
//! ```text
//! cargo run --release -p leviculum-mapcut --features http --example arrive -- \
//!     berlin.pmtiles 13.05,52.32,13.80,52.70 2000000 50 out.lmr
//! ```
//!
//! Arguments: the archive, the box (west,south,east,north in degrees), the
//! link's bytes a second, its latency in milliseconds, the output file,
//! and `whole` to load as before 1291 for the comparison: every tile
//! fetched first, then cut, nothing to draw until the end.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::time::{Duration, Instant};

use leviculum_map::region::{Bounds, Region, Slot};
use leviculum_mapcut::http::HttpRange;
use leviculum_mapcut::load::{cut_planned, plan, Want};
use leviculum_mapcut::pmtiles::Archive;
use leviculum_mapcut::serve::Serve;
use leviculum_mapcut::Res;

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (archive, bbox, rate, latency, out, whole) = match args.as_slice() {
        [a, b, r, l, o] => (a, b, r, l, o, false),
        [a, b, r, l, o, w] if w == "whole" => (a, b, r, l, o, true),
        _ => {
            return Err(
                "arrive <archive> <w,s,e,n> <bytes/s> <latency ms> <out.lmr> [whole]".into(),
            )
        }
    };
    let deg: Vec<f64> = bbox.split(',').map(str::parse).collect::<Result<_, _>>()?;
    let [w, s, e, n] = deg.as_slice() else {
        return Err("the box is west,south,east,north".into());
    };
    let e7 = |d: f64| (d * 1e7).round() as i32;
    let bounds = Bounds {
        west: e7(*w),
        south: e7(*s),
        east: e7(*e),
        north: e7(*n),
    };
    let serve = Serve::start(vec![("a.pmtiles".to_owned(), std::fs::read(archive)?)])?;
    serve.throttle(rate.parse()?, Duration::from_millis(latency.parse()?));
    let started = Instant::now();
    let archive = Archive::from_source(HttpRange::new(&serve.url("a.pmtiles")))?;
    let want = Want {
        bounds,
        zooms: 0..=15,
        floor: 15,
        cap: u64::MAX,
        profile: leviculum_map::profile::CUT_2,
        locale: None,
        buildings: true,
        pois: true,
    };
    if whole {
        let located = archive.locate_in(&want.bounds, want.zooms.clone())?;
        let fetched = archive.fetch(&located)?;
        let opts = leviculum_mapcut::cut::CutOptions {
            bounds,
            zooms: want.zooms.clone(),
            profile: want.profile,
            attribution: "© OpenStreetMap contributors".to_owned(),
            notice: "arrive".to_owned(),
            threads: 4,
            locale: None,
            buildings: true,
            pois: true,
            round: leviculum_mapcut::cut::ROUND,
        };
        let stats =
            leviculum_mapcut::cut::cut(&fetched, &opts, BufWriter::new(File::create(out)?))?;
        println!(
            "ARRIVE whole total_ms={} fetched={} requests={} region_bytes={}",
            started.elapsed().as_millis(),
            fetched.bytes,
            fetched.requests,
            stats.bytes
        );
        return Ok(());
    }
    let planned = plan(&archive, &want)?;
    println!(
        "ARRIVE planned ms={} zooms={:?}",
        started.elapsed().as_millis(),
        planned.zooms
    );
    let mut first_tile: Option<u128> = None;
    let mut zoom_done: BTreeMap<u8, u128> = BTreeMap::new();
    let mut rounds = 0;
    let mut progress = |p: leviculum_mapcut::cut::Progress| {
        rounds += 1;
        let ms = started.elapsed().as_millis();
        let Ok(region) = File::open(out)
            .map_err(|e| e.to_string())
            .and_then(|f| Region::open(f).map_err(|e| format!("{e:?}")))
        else {
            println!("ARRIVE round={rounds} ms={ms} not readable");
            return;
        };
        let mut here = 0;
        let mut pending: BTreeMap<u8, u32> = BTreeMap::new();
        for n in 0..region.header().tiles {
            match region.slot(n) {
                Ok((_, Slot::Here(_))) => here += 1,
                Ok((id, Slot::Pending)) => *pending.entry(id.z).or_default() += 1,
                _ => {}
            }
        }
        if here > 0 && first_tile.is_none() {
            first_tile = Some(ms);
        }
        for z in 0..=15u8 {
            if !pending.contains_key(&z) {
                zoom_done.entry(z).or_insert(ms);
            }
        }
        println!(
            "ARRIVE round={rounds} ms={ms} zoom={} tiles={}/{} bytes={} complete={}",
            p.zoom,
            p.done,
            p.tiles,
            p.bytes,
            region.is_complete()
        );
    };
    let loaded = cut_planned(
        &archive,
        &want,
        planned,
        "arrive",
        4,
        BufWriter::new(File::create(out)?),
        &mut progress,
    )?;
    let total = started.elapsed().as_millis();
    println!(
        "ARRIVE first_tile_ms={} z13_full_ms={} total_ms={total} fetched={} requests={} region_bytes={}",
        first_tile.unwrap_or(0),
        zoom_done.get(&13).copied().unwrap_or(0),
        loaded.fetched,
        loaded.requests,
        loaded.stats.bytes
    );
    for (z, ms) in zoom_done {
        println!("ARRIVE zoom={z} full_ms={ms}");
    }
    Ok(())
}
