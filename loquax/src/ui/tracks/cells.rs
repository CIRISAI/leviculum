//! The tracks in the terminal's cells (`loquax-tracks.md`, "The
//! terminal"): the lines as braille dots, two by four a cell, whatever
//! technique draws the picture under them; a dwell as `•`, a place as `○`,
//! an outlier as `·`, the wedge as an arrow in the cell the newest fix
//! moves towards; the pin's own age says how long it has been silent
//! (1447: the quiet line is gone). A line whose
//! newer fix arrived late (1386) is `┄` where no other line crosses the
//! cell, `:` in ASCII. Four levels of
//! age ([`super::Age::level`]), the hue per peer ([`super::hue`]).
//!
//! A cell holds one track: where two cross, the one drawn later wins, and
//! the stronger level within one track.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_map::viewport::Viewport;
use leviculum_ui::Rect;

use super::{Age, Track};

/// What one cell of the track shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackCell {
    pub col: u16,
    pub row: u16,
    /// The glyph.
    pub text: String,
    /// [`super::hue`]: `None` for our own, grey.
    pub hue: Option<u8>,
    /// 0 to 3, 3 the strongest ([`super::Age::level`]).
    pub level: u8,
}

/// Places, dwells and outliers: what a cell shows besides a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Mark {
    Place,
    Dot,
    Dwell,
    Wedge(char),
}

/// The cells of `tracks` over `area`, the view `view` drawn at `cell_px`
/// image pixels a cell; `ascii` draws lines as `.`, dwells as `o`, places
/// as `O`, outliers as `,`.
pub fn cells(
    tracks: &[Track],
    view: &Viewport,
    area: Rect,
    cell_px: (u16, u16),
    ascii: bool,
) -> Vec<TrackCell> {
    let (cw, ch) = (i64::from(cell_px.0.max(1)), i64::from(cell_px.1.max(1)));
    let (cols, rows) = (i64::from(area.width), i64::from(area.height));
    // A dot of the braille grid a point lands in.
    let dot = |w| {
        let (x, y) = view.to_screen(w);
        (x * 2 / cw, y * 4 / ch)
    };
    let mut out = Vec::new();
    for track in tracks {
        // Per cell: the braille bits, the level, and a mark over them.
        let mut grid: BTreeMap<(i64, i64), (u8, u8, Option<Mark>)> = BTreeMap::new();
        // The cells a line crosses that arrived on time.
        let mut solid = alloc::collections::BTreeSet::new();
        let set_dot = |grid: &mut BTreeMap<(i64, i64), (u8, u8, Option<Mark>)>,
                       (x, y): (i64, i64),
                       level: u8| {
            let (col, row) = (x.div_euclid(2), y.div_euclid(4));
            if !(0..cols).contains(&col) || !(0..rows).contains(&row) {
                return;
            }
            let bit = braille_bit(x.rem_euclid(2), y.rem_euclid(4));
            let cell = grid.entry((col, row)).or_insert((0, 0, None));
            cell.0 |= bit;
            cell.1 = cell.1.max(level);
        };
        let mark = |grid: &mut BTreeMap<(i64, i64), (u8, u8, Option<Mark>)>,
                    (x, y): (i64, i64),
                    m: Mark,
                    level: u8| {
            let (col, row) = (x.div_euclid(2), y.div_euclid(4));
            if !(0..cols).contains(&col) || !(0..rows).contains(&row) {
                return;
            }
            let cell = grid.entry((col, row)).or_insert((0, 0, None));
            cell.2 = cell.2.max(Some(m));
            cell.1 = cell.1.max(level);
        };
        for p in &track.places {
            mark(&mut grid, dot(p.at), Mark::Place, Age::Old.level());
        }
        for s in &track.segments {
            let (a, b) = (dot(s.from), dot(s.to));
            // Off the area by far: not walked dot by dot.
            let off =
                |(x, y): (i64, i64)| x < -cols * 8 || y < -rows * 8 || x > cols * 8 || y > rows * 8;
            if off(a) && off(b) {
                continue;
            }
            for p in line(a, b) {
                set_dot(&mut grid, p, s.age.level());
                if !s.late {
                    solid.insert((p.0.div_euclid(2), p.1.div_euclid(4)));
                }
            }
        }
        for d in &track.dots {
            mark(&mut grid, dot(d.at), Mark::Dot, Age::Old.level());
        }
        for b in &track.dwells {
            mark(&mut grid, dot(b.at), Mark::Dwell, b.age.level());
        }
        if let Some(w) = &track.wedge {
            // The cell beside the pin, the way it moves.
            let (x, y) = dot(w.at);
            let (dc, dr, glyph) = arrow(w.bearing, ascii);
            mark(&mut grid, (x + dc * 2, y + dr * 4), Mark::Wedge(glyph), 3);
        }
        for ((col, row), (bits, level, m)) in grid {
            let glyph = match (m, ascii) {
                (Some(Mark::Wedge(c)), _) => c,
                (Some(Mark::Dwell), false) => '•',
                (Some(Mark::Dwell), true) => 'o',
                (Some(Mark::Dot), false) if bits == 0 => '·',
                (Some(Mark::Dot), true) if bits == 0 => ',',
                (Some(Mark::Place), false) if bits == 0 => '○',
                (Some(Mark::Place), true) if bits == 0 => 'O',
                (None, true) if !solid.contains(&(col, row)) => ':',
                (None, false) if !solid.contains(&(col, row)) => '┄',
                (_, true) => '.',
                (_, false) => char::from_u32(0x2800 + u32::from(bits)).unwrap_or('⠿'),
            };
            out.push(TrackCell {
                col: area.x + col as u16,
                row: area.y + row as u16,
                text: glyph.into(),
                hue: track.hue,
                level,
            });
        }
    }
    out
}

/// The braille bit of the dot `(dx, dy)`, dx in 0..2, dy in 0..4.
fn braille_bit(dx: i64, dy: i64) -> u8 {
    match (dx, dy) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (0, _) => 0x40,
        _ => 0x80,
    }
}

/// The dots of the line from `a` to `b`, both ends in.
fn line(a: (i64, i64), b: (i64, i64)) -> Vec<(i64, i64)> {
    let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
    let steps = dx.max(dy).min(4096);
    (0..=steps)
        .map(|i| match steps {
            0 => a,
            n => (a.0 + (b.0 - a.0) * i / n, a.1 + (b.1 - a.1) * i / n),
        })
        .collect()
}

/// The cell step and the arrow for a bearing in degrees: eight ways.
fn arrow(bearing: i32, ascii: bool) -> (i64, i64, char) {
    let way = ((bearing.rem_euclid(360) + 22) / 45) % 8;
    let (dc, dr, fancy, plain) = match way {
        0 => (0, -1, '↑', '^'),
        1 => (1, -1, '↗', '/'),
        2 => (1, 0, '→', '>'),
        3 => (1, 1, '↘', '\\'),
        4 => (0, 1, '↓', 'v'),
        5 => (-1, 1, '↙', '/'),
        6 => (-1, 0, '←', '<'),
        _ => (-1, -1, '↖', '\\'),
    };
    (dc, dr, if ascii { plain } else { fancy })
}

#[cfg(test)]
mod tests {
    use super::super::{Blob, Place, Segment, Wedge};
    use super::*;
    use alloc::vec;
    use leviculum_map::mercator::World;

    /// A view of 10 by 4 braille cells at z20 about the world's middle;
    /// a world point `(dx, dy)` image pixels from its top left corner.
    fn view() -> Viewport {
        Viewport::new(
            World {
                x: 1 << 31,
                y: 1 << 31,
            },
            20,
            20,
            16,
        )
    }

    fn px(view: &Viewport, x: i32, y: i32) -> World {
        view.to_world(x, y)
    }

    #[test]
    fn a_line_is_braille_and_marks_are_glyphs() {
        let v = view();
        let area = Rect::new(1, 1, 10, 4);
        let track = Track {
            peer: [3; 16],
            hue: Some(3),
            segments: vec![Segment {
                from: px(&v, 0, 1),
                to: px(&v, 19, 1),
                age: Age::Fresh,
                late: false,
            }],
            dots: Vec::new(),
            dwells: vec![Blob {
                at: px(&v, 4, 9),
                radius: 4,
                age: Age::Half,
            }],
            places: vec![Place {
                at: px(&v, 8, 13),
                radius: 20,
                dwell_secs: 86_400,
                label: None,
            }],
            wedge: Some(Wedge {
                at: px(&v, 18, 1),
                bearing: 90,
            }),
            head: Some(px(&v, 0, 1)),
        };
        let got = cells(&[track], &v, area, (2, 4), false);
        let mut frame = vec![vec![' '; 10]; 4];
        for c in &got {
            for (i, ch) in c.text.chars().enumerate() {
                if let Some(cell) =
                    frame[usize::from(c.row - 1)].get_mut(usize::from(c.col - 1) + i)
                {
                    *cell = ch;
                }
            }
        }
        let text: Vec<String> = frame.iter().map(|r| r.iter().collect()).collect();
        // The top row's line in the dots' second row; the wedge at the
        // right end overwrites nothing, it is past the edge.
        assert_eq!(text[0], "⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒");
        assert_eq!(text[1], "          ", "no quiet line under the pin (1447)");
        assert_eq!(text[2], "  •       ");
        assert_eq!(&text[3][..], "    ○     ");
        let dwell = got.iter().find(|c| c.text == "•").expect("dwell");
        assert_eq!((dwell.col, dwell.row, dwell.level), (3, 3, 2));
        assert!(got.iter().all(|c| c.hue == Some(3)));
    }

    #[test]
    fn the_arrow_points_eight_ways() {
        assert_eq!(arrow(0, false), (0, -1, '↑'));
        assert_eq!(arrow(91, false), (1, 0, '→'));
        assert_eq!(arrow(225, true), (-1, 1, '/'));
        assert_eq!(arrow(-10, false).2, '↑');
    }
}
