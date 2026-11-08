//! The goldens' comparison, shared by `golden.rs` and `firmware.rs`: a
//! frame goes through the board's dump (`loquax_m9::dump`) and back, then
//! into a PNG compared in bytes with the file under `tests/golden/`. A
//! frame that moved fails and leaves the new picture in the test's target
//! dir as `m9-golden/<name>`; `LOQUAX_WRITE_GOLDEN=1` writes the goldens
//! instead.

#![allow(dead_code)]

use std::path::PathBuf;

use loquax_m9::dump;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};

/// `frame` dumped the way the board dumps it and read back, with the
/// dump's size in bytes, line ends counted as the board sends them.
pub fn through_dump(frame: &Frame) -> (Frame, usize) {
    let mut text = String::new();
    dump::encode(WIDTH, HEIGHT, |x, y| frame.pixel(x, y), &mut |line| {
        text.push_str(line);
        text.push_str("\r\n");
    });
    let back = dump::decode(&text).expect("the dump reads back");
    assert_eq!((back.width, back.height), (WIDTH, HEIGHT));
    let back = Frame::from_pixels(back.pixels).expect("a whole frame");
    assert_eq!(back.pixels(), frame.pixels(), "the dump changed the frame");
    (back, text.len())
}

/// What the board's flush does to the panel, on `shown`: the runs of rows
/// `panel` changed since its last flush, a window each (1507), copied
/// over. Nothing when nothing changed.
pub fn flush_runs(panel: &Frame, shown: &mut Frame) {
    let runs = panel.take_runs();
    if runs.is_empty() {
        return;
    }
    let mut pixels: Vec<u16> = (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| shown.pixel(x, y))
        .collect();
    for (first, last) in runs {
        for y in first..=last {
            for x in 0..WIDTH {
                pixels[(y * WIDTH + x) as usize] = panel.pixel(x, y);
            }
        }
    }
    *shown = Frame::from_pixels(pixels).expect("whole");
}

pub fn png(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer
        .write_image_data(&frame.to_rgb888())
        .expect("png data");
    writer.finish().expect("png end");
    out
}

/// `frame`, through the dump, against the golden `name`.
pub fn check(name: &str, frame: &Frame) {
    let (frame, _) = through_dump(frame);
    let got = png(&frame);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read(&path).unwrap_or_default();
    if got != want {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m9-golden");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), &got);
        panic!(
            "{name} moved: {} B drawn against {} B pinned; the drawn one is {}",
            got.len(),
            want.len(),
            dir.join(name).display()
        );
    }
}
