//! The 1710 proof pictures and costs: zoom strips and frame series of the
//! world view blending out of Equal Earth and out of the orthographic
//! globe into Mercator, and the draw time of a world view before and
//! after.
//!
//!    cargo run --release -p leviculum-map --example world_blend_pictures -- <outdir>
//!
//! Writes, under `<outdir>`:
//! - `strip-<shape>-<centre>.png`: z1 to z7 every half zoom, 240x160
//!   each, side by side — the ramp to flip through at a glance.
//! - `seq-<shape>-<centre>/z<zzz>.png`: Berlin and Merzouga every tenth
//!   of a zoom, 480x320 — the two frame sequences, both shapes.
//! - `costs.txt`: `[MAP]` lines for the linear drawer and the blend.

use std::fmt::Write as _;
use std::time::Instant;

use leviculum_map::blend::{Blend, Shape};
use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::outline::{self, WORLD};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::style::Style;

const CENTRES: [(&str, f64, f64); 5] = [
    ("null", 0.0, 0.0),
    ("berlin", 52.52, 13.405),
    ("merzouga", 31.08, -4.01),
    ("singapore", 1.35, 103.82),
    ("norway65", 65.0, 13.0),
];

const SHAPES: [(&str, Shape); 2] = [("ee", Shape::EqualEarth), ("globe", Shape::Globe)];

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| String::from("world-blend-pictures"));
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("outdir");
    let style = Style::SCREEN;

    // The strips: z1 to z7 every half zoom.
    for (sname, shape) in SHAPES {
        for (cname, lat, lon) in CENTRES {
            let centre = project(LatLon::from_degrees(lat, lon));
            let (w, h) = (240u32, 160u32);
            let steps: Vec<f64> = (2..=14).map(|i| f64::from(i) / 2.0).collect();
            let mut strip = vec![0u8; steps.len() * (w * h) as usize * 3];
            for (i, &z) in steps.iter().enumerate() {
                let rgb = world_rgb(shape, centre, z, w, h, style);
                let row_px = steps.len() * w as usize;
                for y in 0..h as usize {
                    let src = &rgb[y * w as usize * 3..(y + 1) * w as usize * 3];
                    let at = (y * row_px + i * w as usize) * 3;
                    strip[at..at + src.len()].copy_from_slice(src);
                }
            }
            let name = out.join(format!("strip-{sname}-{cname}.png"));
            write_png(&name, &strip, steps.len() as u32 * w, h);
            println!("wrote {}", name.display());
        }
    }

    // The frame series at Berlin and Merzouga, every tenth of a zoom.
    for (sname, shape) in SHAPES {
        for (cname, lat, lon) in CENTRES[1..3].iter().copied() {
            let centre = project(LatLon::from_degrees(lat, lon));
            let dir = out.join(format!("seq-{sname}-{cname}"));
            std::fs::create_dir_all(&dir).expect("seqdir");
            let (w, h) = (480u32, 320u32);
            for i in 10..=70 {
                let z = f64::from(i) / 10.0;
                let rgb = world_rgb(shape, centre, z, w, h, style);
                write_png(&dir.join(format!("z{i:03}.png")), &rgb, w, h);
            }
            println!("wrote {}", dir.display());
        }
    }

    // The costs: the linear drawer against the blend, host, 480x320 and
    // 1024x768, hot, the median of nine.
    let mut costs = String::new();
    for (w, h) in [(480u32, 320u32), (1024u32, 768u32)] {
        let centre = project(LatLon::from_degrees(50.0, 10.0));
        let linear = median_ms(|| {
            let view = Viewport::new(centre, 2, w, h);
            let mut fb = vec![0u16; (w * h) as usize];
            let mut frame = Frame::new(&mut fb, w, h).expect("frame");
            let Ok(_) = outline::draw_world(&WORLD, &view, Ink::Colour, style.theme, &mut frame);
        });
        let _ = writeln!(costs, "[MAP] world draw mercator {w}x{h} z2 {linear:.1} ms");
        for (sname, shape) in SHAPES {
            for z in [2.0, 3.5] {
                let ms = median_ms(|| {
                    let b = Blend::new(shape, centre, z, w, h);
                    let mut fb = vec![0u16; (w * h) as usize];
                    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
                    let Ok(_) =
                        outline::draw_world_view(&WORLD, &b, Ink::Colour, style.theme, &mut frame);
                });
                let _ = writeln!(costs, "[MAP] world draw {sname} {w}x{h} z{z} {ms:.1} ms");
            }
        }
    }
    print!("{costs}");
    std::fs::write(out.join("costs.txt"), costs).expect("costs");
}

fn world_rgb(shape: Shape, centre: World, z: f64, w: u32, h: u32, style: Style) -> Vec<u8> {
    // Through the blend at every step, the fractional Mercator past the
    // handover included, so the series is one continuous mapping.
    let blend = Blend::new(shape, centre, z, w, h);
    let mut fb = vec![0u16; (w * h) as usize];
    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
    let Ok(_) = outline::draw_world_view(&WORLD, &blend, Ink::Colour, style.theme, &mut frame);
    let mut rgb = Vec::with_capacity(fb.len() * 3);
    let frame = Frame::new(&mut fb, w, h).expect("frame");
    frame.rgb888_into(&mut rgb);
    rgb
}

fn median_ms(mut draw: impl FnMut()) -> f64 {
    let mut times: Vec<f64> = (0..9)
        .map(|_| {
            let t = Instant::now();
            draw();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

fn write_png(path: &std::path::Path, rgb: &[u8], w: u32, h: u32) {
    let mut bytes = Vec::new();
    {
        let mut e = png::Encoder::new(&mut bytes, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut writer = e.write_header().expect("header");
        writer.write_image_data(rgb).expect("data");
        writer.finish().expect("finish");
    }
    std::fs::write(path, bytes).expect("write png");
}
