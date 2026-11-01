//! What a photo costs on this machine (1220): for each picture named,
//! its size, then per photo size the bytes that go and the time
//! [`loquax::photo::prepare`] took, then the time to decode it whole and
//! to make the bubble's 480 px thumbnail of what went. Each time is the
//! best of three runs.
//!
//! `cargo run --release -p loquax --features photos --example photo_cost -- a.jpg b.jpg`

use std::time::{Duration, Instant};

use loquax::photo::{decode, dimensions, fit, prepare, PhotoSize};

fn best<T>(mut run: impl FnMut() -> T) -> (T, Duration) {
    let mut fastest = Duration::MAX;
    let mut out = None;
    for _ in 0..3 {
        let start = Instant::now();
        let value = run();
        fastest = fastest.min(start.elapsed());
        out = Some(value);
    }
    match out {
        Some(value) => (value, fastest),
        None => unreachable!("three runs"),
    }
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("{path}: {error}");
                continue;
            }
        };
        let (w, h) = dimensions(&bytes).unwrap_or((0, 0));
        println!(
            "{path}: {w}x{h} ({:.1} MP), {} bytes",
            f64::from(w) * f64::from(h) / 1e6,
            bytes.len()
        );
        let (_, took) = best(|| decode(&bytes).map(|i| i.width()));
        println!("  decode the original: {} ms", took.as_millis());
        for size in PhotoSize::ALL {
            let (prepared, took) = best(|| prepare(&bytes, size));
            match prepared {
                Ok(p) => {
                    let (_, thumb) = best(|| fit(&p.data, 480).map(|t| t.width));
                    println!(
                        "  {:8} {}x{} {} bytes, prepare {} ms, thumbnail of it {} ms",
                        size.word(),
                        p.width,
                        p.height,
                        p.data.len(),
                        took.as_millis(),
                        thumb.as_millis()
                    );
                }
                Err(error) => println!("  {:8} refused: {error}", size.word()),
            }
        }
    }
}
