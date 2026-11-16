//! The help overlay's model: titled groups of keybindings, and their plain-text
//! projection with every description aligned to one computed column.
//!
//! A program supplies its groups; `leviculum-ui-ratatui` draws them. Both the
//! drawing and the overlay's sizing derive from [`lines`], so they can never
//! disagree and no padding is hand-counted.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::geometry::Rect;

/// One keybinding shown in the help overlay: the keys and their description. The
/// description column is COMPUTED from the widest `keys` across all groups (see
/// [`lines`]), never hand-padded, so it can never misalign.
pub struct HelpEntry {
    /// The keys, as the reader presses them (`Tab / S-Tab`).
    pub keys: &'static str,
    /// What they do.
    pub desc: &'static str,
}

/// A titled group of [`HelpEntry`]s in the help overlay.
pub struct HelpGroup {
    /// The group's title line.
    pub title: &'static str,
    /// Its bindings, in display order.
    pub entries: Vec<HelpEntry>,
}

/// The indent applied to every entry (its keys and description) under a group.
pub const HELP_INDENT: &str = "  ";
/// The gap between the (padded) keys column and the description column.
pub const HELP_GAP: usize = 2;

/// The widest `keys` display width across every entry of every group: the width
/// the keys column is padded to so all descriptions align.
pub fn key_col(groups: &[HelpGroup]) -> usize {
    groups
        .iter()
        .flat_map(|g| g.entries.iter())
        .map(|e| UnicodeWidthStr::width(e.keys))
        .max()
        .unwrap_or(0)
}

/// The plain-text lines of the help overlay: each group's title, then its
/// entries with every description aligned to the computed keys column, with a
/// blank line between groups. The renderer and the layout both derive from this,
/// so widths can never disagree and no padding is hand-counted. The block's own
/// border title carries the heading, so no heading line lives in the body.
pub fn lines(groups: &[HelpGroup]) -> Vec<String> {
    let col = key_col(groups);
    let mut lines = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            lines.push(String::new());
        }
        lines.push(group.title.to_string());
        for e in &group.entries {
            let pad = col.saturating_sub(UnicodeWidthStr::width(e.keys)) + HELP_GAP;
            lines.push(format!(
                "{HELP_INDENT}{}{}{}",
                e.keys,
                " ".repeat(pad),
                e.desc
            ));
        }
    }
    lines
}

/// What a stretch of a help line is: a group's title, an entry's keys (with
/// the indent before them) or its description (with the padding before it).
/// Each renderer styles the three its own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Title,
    Keys,
    Desc,
}

/// [`lines`] as stretches of [`Part`]s, one list per line: joined, each
/// list is its line, character for character.
pub fn segments(groups: &[HelpGroup]) -> Vec<Vec<(String, Part)>> {
    let col = key_col(groups);
    let mut out = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            out.push(Vec::new());
        }
        out.push(alloc::vec![(group.title.to_string(), Part::Title)]);
        for e in &group.entries {
            let pad = col.saturating_sub(UnicodeWidthStr::width(e.keys)) + HELP_GAP;
            out.push(alloc::vec![
                (format!("{HELP_INDENT}{}", e.keys), Part::Keys),
                (format!("{}{}", " ".repeat(pad), e.desc), Part::Desc),
            ]);
        }
    }
    out
}

/// The characters `chars` of a line given as [`segments`], each stretch
/// keeping its part: what one drawn [`HelpRow`] shows.
pub fn cut(segments: &[(String, Part)], chars: Range<usize>) -> Vec<(String, Part)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (text, part) in segments {
        let len = text.chars().count();
        let from = chars.start.max(at);
        let to = chars.end.min(at + len);
        if from < to {
            out.push((
                text.chars().skip(from - at).take(to - from).collect(),
                *part,
            ));
        }
        at += len;
    }
    out
}

/// Where the overlay is drawn over `area`: centred, the widest line plus its
/// two borders wide and every drawn row ([`rows`]) plus the borders high, each
/// cut to the area. The renderer draws here and a program's click hit-tests
/// against the same rectangle, so a click outside it is outside what the
/// reader sees.
pub fn overlay_rect(groups: &[HelpGroup], area: Rect) -> Rect {
    let width = overlay_width(groups, area);
    let drawn = wrap_all(&lines(groups), usize::from(width.saturating_sub(2))).len();
    let height = (drawn as u16 + 2).min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// The overlay's width over `area`: the widest line plus two borders, cut to
/// the area.
fn overlay_width(groups: &[HelpGroup], area: Rect) -> u16 {
    let inner = lines(groups)
        .iter()
        .map(|l| UnicodeWidthStr::width(l.as_str()))
        .max()
        .unwrap_or(0) as u16;
    (inner + 2).min(area.width)
}

/// One row the overlay draws: the index of its line in [`lines`] and the
/// characters of that line it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpRow {
    /// The line in [`lines`].
    pub line: usize,
    /// Its characters on this row, by character index.
    pub chars: Range<usize>,
}

/// The rows the overlay draws over `area`: every line of [`lines`], a line
/// wider than the overlay's inside (a phone's terminal) wrapped after its last
/// space that fits, or cut where none does. The renderer draws exactly these
/// and [`scroll_bounds`] counts them, so the last row stays reachable however
/// narrow the screen.
pub fn rows(groups: &[HelpGroup], area: Rect) -> Vec<HelpRow> {
    let width = usize::from(overlay_width(groups, area).saturating_sub(2));
    wrap_all(&lines(groups), width)
}

fn wrap_all(lines: &[String], width: usize) -> Vec<HelpRow> {
    lines
        .iter()
        .enumerate()
        .flat_map(|(line, text)| {
            wrap(text, width)
                .into_iter()
                .map(move |chars| HelpRow { line, chars })
        })
        .collect()
}

/// `text` cut into rows at most `width` cells wide, as character ranges: each
/// row ends after its last space when the next character would not fit, the
/// spaces at the break are dropped, and a word wider than a row is cut.
fn wrap(text: &str, width: usize) -> Vec<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return alloc::vec![0..0];
    }
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start;
        let mut cells = 0;
        while end < chars.len() {
            let w = UnicodeWidthChar::width(chars[end]).unwrap_or(0);
            if cells + w > width && end > start {
                break;
            }
            cells += w;
            end += 1;
        }
        let mut cut = end;
        if end < chars.len() && chars[end] != ' ' {
            if let Some(space) = (start + 1..=end).rev().find(|&i| chars[i - 1] == ' ') {
                cut = space;
            }
        }
        let mut shown = cut;
        while shown > start + 1 && chars[shown - 1] == ' ' {
            shown -= 1;
        }
        rows.push(start..shown);
        start = cut;
        while start < chars.len() && chars[start] == ' ' {
            start += 1;
        }
    }
    rows
}

/// The overlay's scroll bounds over `area`: the rows it shows at once (its
/// [`overlay_rect`] less the two borders) and the largest valid first row of
/// [`rows`]. The key handler clamps its offset to these and the renderer
/// slices by them, so the two never disagree.
pub fn scroll_bounds(groups: &[HelpGroup], area: Rect) -> (usize, usize) {
    let total = rows(groups, area).len();
    let viewport = usize::from(overlay_rect(groups, area).height.saturating_sub(2));
    (viewport, total.saturating_sub(viewport))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(text: &str, width: usize) -> Vec<String> {
        wrap(text, width)
            .into_iter()
            .map(|r| text.chars().skip(r.start).take(r.len()).collect())
            .collect()
    }

    #[test]
    fn a_long_line_wraps_after_a_space_and_a_long_word_is_cut() {
        assert_eq!(cut("", 10), [""]);
        assert_eq!(cut("short", 10), ["short"]);
        assert_eq!(cut("one two three", 8), ["one two", "three"]);
        assert_eq!(cut("  j / k    scroll", 9), ["  j / k", "scroll"]);
        assert_eq!(cut("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(cut("ab ↑↓ cd", 5), ["ab ↑↓", "cd"]);
    }

    /// The segments joined are the lines, and a row's cut joined is the
    /// row's characters of its line.
    #[test]
    fn the_segments_are_the_lines_and_a_cut_is_the_row() {
        let groups = [HelpGroup {
            title: "Move",
            entries: alloc::vec![
                HelpEntry {
                    keys: "j / k",
                    desc: "line down / up",
                },
                HelpEntry {
                    keys: "g",
                    desc: "top",
                },
            ],
        }];
        let lines = lines(&groups);
        let segs = segments(&groups);
        let joined: Vec<String> = segs
            .iter()
            .map(|l| l.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(joined, lines);
        let area = Rect::new(0, 0, 14, 10);
        for row in rows(&groups, area) {
            let got: String = super::cut(&segs[row.line], row.chars.clone())
                .iter()
                .map(|(t, _)| t.as_str())
                .collect();
            let want: String = lines[row.line]
                .chars()
                .skip(row.chars.start)
                .take(row.chars.len())
                .collect();
            assert_eq!(got, want);
        }
        assert_eq!(
            super::cut(&segs[1], 0..4),
            [("  j ".to_string(), Part::Keys)]
        );
    }

    #[test]
    fn the_bounds_count_the_wrapped_rows() {
        let groups = [HelpGroup {
            title: "Move",
            entries: alloc::vec![
                HelpEntry {
                    keys: "j / k",
                    desc: "scroll a line, a description long enough to wrap",
                },
                HelpEntry {
                    keys: "g / G",
                    desc: "top / bottom",
                },
            ],
        }];
        let wide = Rect::new(0, 0, 200, 4);
        assert_eq!(rows(&groups, wide).len(), 3);
        assert_eq!(scroll_bounds(&groups, wide), (2, 1));
        let narrow = Rect::new(0, 0, 30, 4);
        let drawn = rows(&groups, narrow).len();
        assert!(drawn > 3, "{drawn}");
        assert_eq!(scroll_bounds(&groups, narrow), (2, drawn - 2));
    }
}
