//! What a voice message costs (1222): the bytes a second of speech takes
//! and the time to encode and decode a minute of it, for the reviewer and
//! the concept page. With a path, it also writes the minute's Ogg file
//! there for `opusinfo`. Never in a package.

use std::time::Instant;

fn main() {
    let minute = loquax::voice::test_speech(16_000, 60_000);
    let mut best = (f64::MAX, f64::MAX);
    let mut ogg = Vec::new();
    for _ in 0..3 {
        let at = Instant::now();
        ogg = match loquax::voice::encode(&minute, 16_000) {
            Ok(ogg) => ogg,
            Err(error) => panic!("encode: {error}"),
        };
        let encoded = at.elapsed().as_secs_f64();
        let at = Instant::now();
        let back = loquax::voice::decode(loquax::ui::voice::AM_OPUS_OGG, &ogg);
        let decoded = at.elapsed().as_secs_f64();
        assert!(back.is_ok());
        best = (best.0.min(encoded), best.1.min(decoded));
    }
    println!(
        "opus 16 kHz {} b/s: {} bytes a minute ({} B/s), encode {:.0} ms, decode {:.0} ms (best of 3)",
        loquax::voice::BITRATE,
        ogg.len(),
        ogg.len() / 60,
        best.0 * 1000.0,
        best.1 * 1000.0
    );
    let codec2 = vec![0u8; 6 * 50 * 60];
    let at = Instant::now();
    let back = loquax::voice::decode(loquax::ui::voice::AM_CODEC2_2400, &codec2);
    println!(
        "codec2 2400: {} bytes a minute, decode {:.0} ms",
        codec2.len(),
        at.elapsed().as_secs_f64() * 1000.0
    );
    assert!(back.is_ok());
    if let Some(path) = std::env::args().nth(1) {
        if let Err(error) = std::fs::write(&path, &ogg) {
            panic!("{path}: {error}");
        }
    }
}
