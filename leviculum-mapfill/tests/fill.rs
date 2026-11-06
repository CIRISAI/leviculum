//! Pixel-exact checks of the filler and the stroker against small reference
//! images: `#` is a filled pixel, `.` an empty one.

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use leviculum_mapfill::coverage::Coverage;
use leviculum_mapfill::fill::{fill_rings, Edge, FillError, FillRule, Filler, Scratch};
use leviculum_mapfill::stroke::{isqrt, stroke, stroke_pen, Pen};
use leviculum_mapfill::SUBPIXEL;

/// Counts how often each pixel was painted, so overlaps show as well as gaps.
struct Canvas {
    w: u32,
    h: u32,
    hits: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            hits: vec![0; (w * h) as usize],
        }
    }

    fn picture(&self) -> String {
        let mut s = String::new();
        for y in 0..self.h {
            for x in 0..self.w {
                s.push(match self.hits[(y * self.w + x) as usize] {
                    0 => '.',
                    1 => '#',
                    _ => '2',
                });
            }
            s.push('\n');
        }
        s
    }
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
}

impl DrawTarget for Canvas {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(
        &mut self,
        it: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, _) in it {
            assert!(
                p.x >= 0 && p.y >= 0 && (p.x as u32) < self.w && (p.y as u32) < self.h,
                "filler drew outside the clip at {p:?}"
            );
            self.hits[(p.y as u32 * self.w + p.x as u32) as usize] += 1;
        }
        Ok(())
    }
}

impl Coverage for Canvas {}

/// What each pixel was covered by, in 256ths, when the filler blends its
/// span ends: 256 for a pixel set hard. A pixel touched twice panics.
struct Cover {
    w: u32,
    h: u32,
    alpha: Vec<u16>,
}

impl Cover {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            alpha: vec![0; (w * h) as usize],
        }
    }

    fn put(&mut self, x: i32, y: i32, a: u16) {
        assert!(
            x >= 0 && y >= 0 && (x as u32) < self.w && (y as u32) < self.h,
            "outside at {x},{y}"
        );
        let at = (y as u32 * self.w + x as u32) as usize;
        assert_eq!(self.alpha[at], 0, "({x}, {y}) touched twice");
        self.alpha[at] = a;
    }

    fn row(&self, y: u32) -> &[u16] {
        &self.alpha[(y * self.w) as usize..((y + 1) * self.w) as usize]
    }
}

impl OriginDimensions for Cover {
    fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
}

impl DrawTarget for Cover {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(
        &mut self,
        it: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, _) in it {
            self.put(p.x, p.y, 256);
        }
        Ok(())
    }
}

impl Coverage for Cover {
    fn smooth(&self) -> bool {
        true
    }

    fn blend(&mut self, x: i32, y: i32, _: BinaryColor, alpha: u8) -> Result<(), Self::Error> {
        assert!(alpha > 0 && alpha < 255, "a blend of {alpha}");
        self.put(x, y, u16::from(alpha));
        Ok(())
    }
}

/// 1422: a span's end pixels are blended by the share of them it covers,
/// the pixels between set hard, each pixel once: across a slanted edge the
/// coverage of a row sums to the span's width, the first and last pixel
/// partly covered where the edge crosses them, and a target that does not
/// blend gets the hard spans it always got.
#[test]
fn span_ends_are_blended_by_their_coverage() {
    // A parallelogram: left edge from (2, 0) to (6, 8), right edge from
    // (10.5, 0) to (14.5, 8), so every row's span is 8.5 pixels wide.
    let ring = [
        Point::new(2 * SUBPIXEL, 0),
        Point::new(21 * SUBPIXEL / 2, 0),
        Point::new(29 * SUBPIXEL / 2, 8 * SUBPIXEL),
        Point::new(6 * SUBPIXEL, 8 * SUBPIXEL),
    ];
    let mut st = Store::new(64, 8);
    let mut c = Cover::new(20, 8);
    let r = fill_rings(
        &mut st.scratch(),
        c.bounding_box(),
        &[&ring[..]],
        &mut c,
        BinaryColor::On,
        FillRule::NonZero,
    );
    assert!(r.is_ok());
    for y in 0..8u32 {
        let row = c.row(y);
        let sum: u32 = row.iter().map(|&a| u32::from(a)).sum();
        // 8.5 pixels, give or take a 256th at each blended end.
        assert!((sum as i32 - 8 * 256 - 128).abs() <= 2, "row {y}: {row:?}");
        let partial = row.iter().filter(|&&a| a > 0 && a < 256).count();
        assert!((1..=2).contains(&partial), "row {y}: {row:?}");
        // The left end at x = 2 + (y + 0.5) / 2: a fraction of it covered.
        let left = (2.0 + (f64::from(y) + 0.5) / 2.0) as usize;
        assert!(row[left] > 0 && row[left] < 256, "row {y}: {row:?}");
        assert!(row[..left].iter().all(|&a| a == 0));
    }
    // The same ring into a target that does not blend: the hard spans.
    let mut hard = Canvas::new(20, 8);
    let mut st = Store::new(64, 8);
    let _ = fill_rings(
        &mut st.scratch(),
        hard.bounding_box(),
        &[&ring[..]],
        &mut hard,
        BinaryColor::On,
        FillRule::NonZero,
    );
    assert!(hard.hits.iter().all(|&h| h <= 1));
    let filled: u32 = hard.hits.iter().map(|&h| u32::from(h)).sum();
    assert!((8 * 8..=8 * 9).contains(&filled), "{filled}");
}

/// A point at whole pixel coordinates, in sub-pixels.
fn p(x: i32, y: i32) -> Point {
    Point::new(x * SUBPIXEL, y * SUBPIXEL)
}

struct Store {
    edges: Vec<Edge>,
    heads: Vec<u16>,
    active: Vec<u16>,
}

impl Store {
    fn new(edges: usize, rows: usize) -> Self {
        Self {
            edges: vec![Edge::default(); edges],
            heads: vec![0; rows],
            active: vec![0; edges],
        }
    }

    fn scratch(&mut self) -> Scratch<'_> {
        Scratch {
            edges: &mut self.edges,
            heads: &mut self.heads,
            active: &mut self.active,
        }
    }
}

fn draw(w: u32, h: u32, rings: &[&[Point]], rule: FillRule) -> String {
    let mut c = Canvas::new(w, h);
    let mut st = Store::new(64, h as usize);
    let clip = c.bounding_box();
    fill_rings(
        &mut st.scratch(),
        clip,
        rings,
        &mut c,
        BinaryColor::On,
        rule,
    )
    .expect("fill");
    c.picture()
}

fn both(w: u32, h: u32, rings: &[&[Point]]) -> String {
    let a = draw(w, h, rings, FillRule::EvenOdd);
    let b = draw(w, h, rings, FillRule::NonZero);
    assert_eq!(a, b, "the two rules disagree where they must not");
    a
}

fn pic(rows: &[&str]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

#[test]
fn square() {
    let ring = [p(1, 1), p(5, 1), p(5, 4), p(1, 4)];
    assert_eq!(
        both(6, 5, &[&ring]),
        pic(&["......", ".####.", ".####.", ".####.", "......"])
    );
}

#[test]
fn square_with_hole() {
    // The hole runs the other way, as MVT writes interior rings.
    let outer = [p(0, 0), p(6, 0), p(6, 6), p(0, 6)];
    let hole = [p(2, 2), p(2, 4), p(4, 4), p(4, 2)];
    assert_eq!(
        both(6, 6, &[&outer, &hole]),
        pic(&["######", "######", "##..##", "##..##", "######", "######"])
    );
}

#[test]
fn self_touching_ring() {
    // One ring through the shared corner (3, 3) twice: two squares.
    let ring = [
        p(0, 0),
        p(3, 0),
        p(3, 3),
        p(6, 3),
        p(6, 6),
        p(3, 6),
        p(3, 3),
        p(0, 3),
    ];
    assert_eq!(
        both(6, 6, &[&ring]),
        pic(&["###...", "###...", "###...", "...###", "...###", "...###"])
    );
}

#[test]
fn degenerate_rings_fill_nothing() {
    let there_and_back = [p(1, 1), p(4, 4), p(1, 1)];
    let collinear = [p(0, 2), p(2, 2), p(5, 2)];
    let single = [p(3, 3)];
    let empty = pic(&[".....", ".....", ".....", ".....", "....."]);
    assert_eq!(both(5, 5, &[&there_and_back]), empty);
    assert_eq!(both(5, 5, &[&collinear]), empty);
    assert_eq!(both(5, 5, &[&single]), empty);
    assert_eq!(both(5, 5, &[]), empty);
}

#[test]
fn diagonal_samples_pixel_centres() {
    // The pixel (x, y) is in when x + 0.5 + y + 0.5 < 6; a centre exactly
    // on the hypotenuse, x + y = 5, is on a right edge and so out.
    let tri = [p(0, 0), p(6, 0), p(0, 6)];
    assert_eq!(
        both(6, 6, &[&tri]),
        pic(&["#####.", "####..", "###...", "##....", "#.....", "......"])
    );
}

#[test]
fn edges_beyond_the_clip() {
    // Off the top left, and a ring whose right side lies wholly right of
    // the clip, so the filler never stores its closing edge.
    let top_left = [p(-3, -2), p(3, -2), p(3, 2), p(-3, 2)];
    let right = [p(4, -10), p(100, -10), p(100, 3), p(4, 3)];
    assert_eq!(
        both(6, 5, &[&top_left, &right]),
        pic(&["###.##", "###.##", "....##", "......", "......"])
    );
    // A clip that does not start at the origin.
    let mut c = Canvas::new(6, 5);
    let mut st = Store::new(16, 5);
    let clip = Rectangle::new(Point::new(1, 1), Size::new(3, 2));
    let all = [p(-5, -5), p(20, -5), p(20, 20), p(-5, 20)];
    fill_rings(
        &mut st.scratch(),
        clip,
        &[&all],
        &mut c,
        BinaryColor::On,
        FillRule::NonZero,
    )
    .expect("fill");
    assert_eq!(
        c.picture(),
        pic(&["......", ".###..", ".###..", "......", "......"])
    );
}

#[test]
fn fill_rules_differ_on_overlap() {
    // Two squares, same orientation, overlapping in the middle.
    let a = [p(0, 0), p(4, 0), p(4, 4), p(0, 4)];
    let b = [p(2, 2), p(6, 2), p(6, 6), p(2, 6)];
    assert_eq!(
        draw(6, 6, &[&a, &b], FillRule::EvenOdd),
        pic(&["####..", "####..", "##..##", "##..##", "..####", "..####"])
    );
    assert_eq!(
        draw(6, 6, &[&a, &b], FillRule::NonZero),
        pic(&["####..", "####..", "######", "######", "..####", "..####"])
    );
    // A same-direction inner ring is a hole only under even-odd.
    let outer = [p(0, 0), p(6, 0), p(6, 6), p(0, 6)];
    let inner = [p(2, 2), p(4, 2), p(4, 4), p(2, 4)];
    assert_eq!(
        draw(6, 6, &[&outer, &inner], FillRule::EvenOdd),
        pic(&["######", "######", "##..##", "##..##", "######", "######"])
    );
    assert_eq!(
        draw(6, 6, &[&outer, &inner], FillRule::NonZero),
        pic(&["######", "######", "######", "######", "######", "######"])
    );
}

#[test]
fn shared_edges_neither_overlap_nor_gap() {
    // A square cut along both diagonals into four triangles, each filled
    // on its own: every pixel painted exactly once.
    let (w, h) = (16u32, 16u32);
    let mut c = Canvas::new(w, h);
    let mut st = Store::new(16, h as usize);
    let clip = c.bounding_box();
    let m = Point::new(8 * SUBPIXEL + 5, 8 * SUBPIXEL - 3);
    let corners = [p(0, 0), p(16, 0), p(16, 16), p(0, 16)];
    for i in 0..4 {
        let tri = [corners[i], corners[(i + 1) % 4], m];
        fill_rings(
            &mut st.scratch(),
            clip,
            &[&tri],
            &mut c,
            BinaryColor::On,
            FillRule::EvenOdd,
        )
        .expect("fill");
    }
    assert!(c.hits.iter().all(|&n| n == 1), "{}", c.picture());
}

#[test]
fn overflow_is_reported_and_draws_nothing() {
    let mut c = Canvas::new(6, 5);
    let mut st = Store::new(1, 5);
    let ring = [p(1, 1), p(5, 1), p(5, 4), p(1, 4)];
    let clip = c.bounding_box();
    let r = fill_rings(
        &mut st.scratch(),
        clip,
        &[&ring],
        &mut c,
        BinaryColor::On,
        FillRule::EvenOdd,
    );
    assert_eq!(r, Err(FillError::Overflow));
    assert!(c.hits.iter().all(|&n| n == 0));
}

#[test]
fn wide_lines() {
    // One pixel wide from x 1.5 to 4.5, square caps of half a pixel: the
    // quad runs from x 1 to 5.
    let mut c = Canvas::new(7, 5);
    let mut st = Store::new(64, 5);
    let clip = c.bounding_box();
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, clip);
    let half = SUBPIXEL / 2;
    let line = [
        Point::new(SUBPIXEL + half, 2 * SUBPIXEL + half),
        Point::new(4 * SUBPIXEL + half, 2 * SUBPIXEL + half),
    ];
    let stats = stroke(&mut f, &line, SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&[".......", ".......", ".####..", ".......", "......."])
    );
    assert_eq!(stats.pixels, 4);
    // Three pixels wide around a right-angle joint: the caps close it.
    let mut c = Canvas::new(7, 7);
    let mut st = Store::new(64, 7);
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, c.bounding_box());
    let corner = [
        Point::new(half, 3 * SUBPIXEL + half),
        Point::new(3 * SUBPIXEL + half, 3 * SUBPIXEL + half),
        Point::new(3 * SUBPIXEL + half, 7 * SUBPIXEL),
    ];
    stroke(&mut f, &corner, 3 * SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&[".......", ".......", "#####..", "#####..", "#####..", "..###..", "..###.."])
    );
}

/// Stroke `line` with `pen` on a `w` x `h` canvas.
fn pen_picture(w: u32, h: u32, line: &[Point], pen: Pen) -> String {
    let mut c = Canvas::new(w, h);
    let mut st = Store::new(256, h as usize);
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, c.bounding_box());
    stroke_pen(&mut f, line, pen, &mut c, BinaryColor::On).expect("stroke");
    c.picture()
}

#[test]
fn round_caps_and_joins_add_to_the_quads() {
    // Five pixels wide from x 4.5 to 8.5: flat-ended quad, a disc of
    // radius 2.5 at each end. A disc turning against the quad would cancel
    // its winding where the two overlap and leave a hole; every pixel is
    // painted exactly once.
    let half = SUBPIXEL / 2;
    let round = |width| Pen {
        width,
        round: true,
        dash: None,
    };
    let line = [
        Point::new(4 * SUBPIXEL + half, 4 * SUBPIXEL + half),
        Point::new(8 * SUBPIXEL + half, 4 * SUBPIXEL + half),
    ];
    assert_eq!(
        pen_picture(13, 9, &line, round(5 * SUBPIXEL)),
        pic(&[
            ".............",
            ".............",
            "...#######...",
            "..#########..",
            "..#########..",
            "..#########..",
            "...#######...",
            ".............",
            ".............",
        ])
    );
    // Three pixels wide around a right angle: the outer corner is round
    // where square caps fill it (`wide_lines`).
    let corner = [
        Point::new(half, 3 * SUBPIXEL + half),
        Point::new(3 * SUBPIXEL + half, 3 * SUBPIXEL + half),
        Point::new(3 * SUBPIXEL + half, 7 * SUBPIXEL),
    ];
    assert_eq!(
        pen_picture(7, 7, &corner, round(3 * SUBPIXEL)),
        pic(&[".......", ".......", "####...", "#####..", "#####..", "..###..", "..###.."])
    );
}

#[test]
fn dashes_follow_the_pattern_across_vertices() {
    // One pixel wide, three on and two off, flat ends; the pattern runs on
    // through the vertex at x 7.5 instead of starting again there. The
    // line is 15 pixels long, so the fourth dash would start at its end.
    let half = SUBPIXEL / 2;
    let y = SUBPIXEL + half;
    let line = [
        Point::new(half, y),
        Point::new(7 * SUBPIXEL + half, y),
        Point::new(15 * SUBPIXEL + half, y),
    ];
    let pen = Pen {
        width: SUBPIXEL,
        round: false,
        dash: Some((3 * SUBPIXEL, 2 * SUBPIXEL)),
    };
    assert_eq!(
        pen_picture(16, 3, &line, pen),
        pic(&["................", "###..###..###...", "................"])
    );
}

#[test]
fn stroke_refills_when_the_scratch_runs_out() {
    // Room for one quad at a time: every segment still lands, and where two
    // caps overlap the pixel is painted twice (`2`), harmless for an opaque
    // colour. With room for all quads the same line paints each pixel once.
    let mut c = Canvas::new(10, 3);
    let mut st = Store::new(4, 3);
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, c.bounding_box());
    let y = SUBPIXEL + SUBPIXEL / 2;
    let line: Vec<Point> = (0..5)
        .map(|i| Point::new(2 * i * SUBPIXEL + SUBPIXEL / 2, y))
        .collect();
    stroke(&mut f, &line, SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&["..........", "##2#2#2##.", ".........."])
    );
    let mut c = Canvas::new(10, 3);
    let mut st = Store::new(64, 3);
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, c.bounding_box());
    stroke(&mut f, &line, SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&["..........", "#########.", ".........."])
    );
}

#[test]
fn isqrt_is_floor_sqrt() {
    for v in [
        0u64,
        1,
        2,
        3,
        4,
        15,
        16,
        17,
        99,
        100,
        1 << 40,
        (1 << 40) - 1,
        u64::MAX,
    ] {
        let r = isqrt(v);
        assert!(u128::from(r) * u128::from(r) <= u128::from(v));
        assert!(u128::from(r + 1) * u128::from(r + 1) > u128::from(v));
    }
}
