//! Host cost of ground heights through the Android facade (order 1758):
//! the committed Erg Chebbi pack (`leviculum-map/fixtures/erg-chebbi.lmh`,
//! a real GLO-30 cut, z12 tiles in one z11 block) opened by
//! [`loquax_android::heights::HeightPack`], every stored tile decoded and
//! every derived tile computed, each timed over `ROUNDS` rounds; one line
//! a zoom with the per-tile mean and the fastest round.
//!
//! ```sh
//! cargo run --release -p loquax-android --example height_decode
//! ```

use std::time::Instant;

use leviculum_map::elevation::TILE_CELLS;
use loquax_android::heights::HeightPack;

const CHEBBI: &[u8] = include_bytes!("../../leviculum-map/fixtures/erg-chebbi.lmh");
const ROUNDS: u32 = 50;

fn main() -> Result<(), String> {
    let t0 = Instant::now();
    let mut pack = HeightPack::open(CHEBBI.to_vec())?;
    println!(
        "HEIGHTS open bytes={} us={:.1}",
        CHEBBI.len(),
        t0.elapsed().as_secs_f64() * 1e6
    );
    let mut out = vec![0i16; TILE_CELLS];
    for zoom in pack.zooms().rev() {
        let tiles = pack.tiles(zoom);
        let what = if zoom == *pack.zooms().end() {
            "decode"
        } else {
            "derive"
        };
        let mut best = f64::MAX;
        let mut total = 0.0;
        for _ in 0..ROUNDS {
            let t = Instant::now();
            for &(x, y) in &tiles {
                pack.heights(zoom, x, y, &mut out)?;
            }
            let ms = t.elapsed().as_secs_f64() * 1e3 / tiles.len() as f64;
            best = best.min(ms);
            total += ms;
        }
        println!(
            "HEIGHTS {what} zoom={zoom} tiles={} rounds={ROUNDS} per_tile_ms_mean={:.3} per_tile_ms_best={:.3}",
            tiles.len(),
            total / f64::from(ROUNDS),
            best
        );
    }
    Ok(())
}
