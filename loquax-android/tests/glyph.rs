//! The phone's delivery marks (1232): `Glyph.java` draws the shapes of
//! `loquax::ui::chat` number for number, and drawn the way the phone draws
//! them, at 16 dp on a 3x screen, the two ticks stay two.
//!
//! `LOQUAX_GLYPH_PNG=<path>` writes the five marks in both themes, on our
//! bubble and on the list's ground, rendered at 16 dp on a 3x screen and
//! shown five times enlarged, for a look by eye.

use loquax::ui::chat::{self, Colours, Mark, Shape, GRID, MARKS};

const GLYPH_JAVA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/android/src/network/leviculum/loquax/Glyph.java"
);

/// A number as Java's source writes a float argument: `6`, `1.5f`.
fn java(v: f32) -> String {
    match v.fract() == 0.0 {
        true => format!("{}", v as i32),
        false => format!("{v}f"),
    }
}

/// The Java statements a shape is drawn with.
fn statements(shape: &Shape) -> Vec<String> {
    // A width is a float literal even when whole: `2f`.
    let mut out = vec![format!("setStrokeWidth({}f)", shape.width)];
    if let Some(c) = shape.disc {
        out.push(format!(
            "drawCircle({}, {}, {}, fill)",
            java(c.x),
            java(c.y),
            java(c.r)
        ));
    }
    if let Some(c) = shape.ring {
        out.push(format!(
            "drawCircle({}, {}, {}, stroke)",
            java(c.x),
            java(c.y),
            java(c.r)
        ));
    }
    for line in shape.lines {
        let xy: Vec<String> = line.iter().flat_map(|&(x, y)| [java(x), java(y)]).collect();
        out.push(match line.len() {
            2 => format!("drawLine({}, stroke)", xy.join(", ")),
            _ => format!("path({})", xy.join(", ")),
        });
    }
    if let Some(c) = shape.dot {
        out.push(format!(
            "drawCircle({}, {}, {}, fill)",
            java(c.x),
            java(c.y),
            java(c.r)
        ));
    }
    out
}

/// Each mark's case in `Glyph.java` holds its shape's statements, and the
/// facade's words map to the five kinds.
#[test]
fn the_phone_draws_the_shapes_of_the_core() {
    let source = std::fs::read_to_string(GLYPH_JAVA).expect("Glyph.java");
    let draw = &source[source.find("public void draw(").expect("draw")..];
    let case = |kind: &str| -> String {
        let start = draw
            .find(&format!("            case {kind}:"))
            .unwrap_or_else(|| panic!("no case {kind}"));
        let rest = &draw[start..];
        let end = rest.find("break;").expect("its break");
        rest[..end].to_string()
    };
    for mark in MARKS {
        let kind = match mark {
            Mark::Sending | Mark::Unconfirmed => "CLOCK",
            Mark::Mailbox | Mark::Arrived => "TICK",
            Mark::Delivered => "TICKS",
            Mark::Read => "TICKS_READ",
            Mark::Failed => "FAILED",
        };
        let body = case(kind);
        let shape = mark.shape();
        for statement in statements(shape) {
            // TICKS and TICKS_READ share one case and pick their width.
            let width = statement.starts_with("setStrokeWidth");
            let shared = matches!(mark, Mark::Delivered | Mark::Read) && width;
            let holds = match shared {
                true => body.contains(&format!("{}f", shape.width)),
                false => body.contains(&statement),
            };
            assert!(holds, "case {kind} lacks {statement}:\n{body}");
        }
        assert!(
            source.contains(&format!("case \"{}\": return Kind.{kind};", mark.word()))
                || matches!(mark, Mark::Sending | Mark::Unconfirmed),
            "Glyph.mark has no {:?}",
            mark.word()
        );
        assert!(
            source.contains(&format!("R.id.{}", mark.resource())),
            "Glyph.resource has no {}",
            mark.resource()
        );
    }
    assert!(source.contains("default: return Kind.CLOCK;"));
}

/// The coverage of pixel `(px, py)` of a `side` pixel square by `shape`'s
/// strokes, its ring, its disc or its dot (`part` picks which), 4 x 4
/// samples a pixel.
fn coverage(shape: &Shape, side: f32, px: u32, py: u32, part: Part) -> f32 {
    let u = side / GRID;
    let mut hits = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / u;
            let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / u;
            let half = shape.width / 2.0;
            let inside = match part {
                Part::Disc => shape.disc.is_some_and(|c| (x - c.x).hypot(y - c.y) <= c.r),
                Part::Ink => {
                    shape
                        .ring
                        .is_some_and(|c| ((x - c.x).hypot(y - c.y) - c.r).abs() <= half)
                        || shape.lines.iter().any(|line| {
                            line.windows(2)
                                .any(|w| to_segment((x, y), w[0], w[1]) <= half)
                        })
                        || shape.dot.is_some_and(|c| (x - c.x).hypot(y - c.y) <= c.r)
                }
            };
            hits += usize::from(inside);
        }
    }
    hits as f32 / 16.0
}

#[derive(Clone, Copy)]
enum Part {
    Disc,
    Ink,
}

fn to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (a.0 + t * dx - p.0).hypot(a.1 + t * dy - p.1)
}

/// The pieces of a mark drawn at `side` pixels: pixels at least half
/// covered, joined by their edges.
fn pieces(shape: &Shape, side: u32) -> usize {
    let n = side as usize;
    let on: Vec<bool> = (0..n * n)
        .map(|i| {
            coverage(
                shape,
                side as f32,
                (i % n) as u32,
                (i / n) as u32,
                Part::Ink,
            ) >= 0.5
        })
        .collect();
    let mut seen = vec![false; n * n];
    let mut count = 0;
    for start in 0..n * n {
        if !on[start] || seen[start] {
            continue;
        }
        count += 1;
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % n, i / n);
            let mut next = Vec::new();
            if x > 0 {
                next.push(i - 1);
            }
            if x + 1 < n {
                next.push(i + 1);
            }
            if y > 0 {
                next.push(i - n);
            }
            if y + 1 < n {
                next.push(i + n);
            }
            for j in next {
                if on[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    count
}

/// At 16 dp on a 3x screen (48 pixels) and on a 2x one (32), delivered's
/// and read's ticks are two pieces, the single tick one: the gap of the
/// study's first drawing (3.4) closes into one shape, this one does not.
#[test]
fn two_ticks_stay_two_at_sixteen_dp() {
    for side in [32, 48] {
        assert_eq!(pieces(&chat::TICK, side), 1, "tick at {side}");
        assert_eq!(pieces(&chat::TICKS, side), 2, "delivered at {side}");
        assert_eq!(pieces(&chat::TICKS_READ, side), 2, "read at {side}");
    }
    // The positive control: the old drawing's second tick, its short leg
    // from (10.5, 15), runs into the first at read's stroke.
    let old = Shape {
        lines: &[
            &[(1.5, 12.5), (6.0, 17.0), (15.5, 7.0)],
            &[(10.5, 15.0), (12.5, 17.0), (22.0, 7.0)],
        ],
        ..chat::TICKS_READ
    };
    assert_eq!(pieces(&old, 48), 1);
}

fn argb(c: u32) -> [f32; 3] {
    [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff].map(|v| v as f32)
}

fn over(under: [f32; 3], ink: [f32; 3], a: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| under[i] * (1.0 - a) + ink[i] * a)
}

/// One mark at `side` pixels over `ground`, as the phone composes it.
fn render(mark: Mark, side: u32, ground: u32, ink: u32, failed: (u32, u32)) -> Vec<[f32; 3]> {
    let shape = mark.shape();
    let sign = match mark {
        Mark::Failed => failed.1,
        _ => ink,
    };
    (0..side * side)
        .map(|i| {
            let (x, y) = (i % side, i / side);
            let disc = coverage(shape, side as f32, x, y, Part::Disc);
            let pen = coverage(shape, side as f32, x, y, Part::Ink);
            over(over(argb(ground), argb(failed.0), disc), argb(sign), pen)
        })
        .collect()
}

#[test]
fn the_marks_as_a_picture() {
    let Some(path) = std::env::var_os("LOQUAX_GLYPH_PNG") else {
        return;
    };
    const SIDE: u32 = 48;
    const ZOOM: u32 = 5;
    const PAD: u32 = 16;
    let cell = SIDE * ZOOM + PAD;
    let rows: Vec<(Colours, bool)> = vec![
        (chat::LIGHT, true),
        (chat::LIGHT, false),
        (chat::DARK, true),
        (chat::DARK, false),
    ];
    // Five glyphs: sent-unconfirmed draws sending's clock.
    let marks: Vec<Mark> = MARKS
        .into_iter()
        .filter(|&m| m != Mark::Unconfirmed)
        .collect();
    let (w, h) = (
        cell * marks.len() as u32 + PAD,
        cell * rows.len() as u32 + PAD,
    );
    let mut rgb = vec![0x80u8; (w * h * 3) as usize];
    for (r, (c, bubble)) in rows.iter().enumerate() {
        for (m, &mark) in marks.iter().enumerate() {
            let (ground, ink, failed) = match bubble {
                true => (
                    c.bubble_ours,
                    chat::bubble_ink(mark, c),
                    (c.failed_ground, c.failed_mark),
                ),
                false => (
                    c.background,
                    chat::list_ink(mark, c),
                    (c.list_failed_ground, c.list_failed_mark),
                ),
            };
            let px = render(mark, SIDE, ground, ink, failed);
            let (x0, y0) = (PAD + m as u32 * cell, PAD + r as u32 * cell);
            for y in 0..SIDE * ZOOM {
                for x in 0..SIDE * ZOOM {
                    let p = px[((y / ZOOM) * SIDE + x / ZOOM) as usize];
                    let at = (((y0 + y) * w + x0 + x) * 3) as usize;
                    rgb[at..at + 3].copy_from_slice(&p.map(|v| v.round() as u8));
                }
            }
        }
    }
    let file = std::fs::File::create(&path).expect("the picture");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a header");
    writer.write_image_data(&rgb).expect("the pixels");
}
