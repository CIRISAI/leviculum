//! A chat read by the line on the key targets (1529): the pager with a
//! focus. The terminal and the M9 scroll a conversation one line a press,
//! and the selected message follows the lines; the window and the phone
//! scroll freely and keep their own selection rules, and the chat list
//! stays a row an entry.
//!
//! The rule is [`step`], over the lines a target laid: each message's
//! [`Lines`] among the target's rows (the terminal's laid cache, 1490; the
//! M9's own bubbles over the scene's window), the rows on screen and the
//! view's first row. It knows no target:
//!
//! - Down moves the view one line while the selected message still has
//!   lines below the screen; once its last line is on screen the next
//!   press selects the next message if its first line is on screen, and
//!   moves the view a line otherwise (a separator, a date line between).
//!   The view never moves more than a line a press, so no line is
//!   skipped. Up is the same the other way: a long message is entered
//!   from its end, line by line, never at its start.
//! - A page (Left and Right in the chat, the page keys, `Space`, `Ctrl-f`
//!   and `Ctrl-b`) moves the view a screen less one line of overlap, half
//!   a page half the screen; the selection moves to the first message the
//!   page brought lines of into view (the last, up), never back the way
//!   the page went. A page that reaches the bottom or the top selects
//!   the newest or the first message.
//! - Top and bottom are the first and the newest message, the view at
//!   their end of the chat.
//! - A selection the reader scrolled off screen (the wheel) is not walked
//!   back to: the next line press selects the message the screen shows
//!   first (down) or last (up) and moves nothing else.
//!
//! A message counts as read when its last line has been on screen
//! ([`last_line_shown`], the terminal's viewport, `hit::viewport`), not
//! when its bubble was selected.

use leviculum_ui::keymap::ScrollCmd;

/// One message's lines among the rows a target laid: `start..end`, the
/// separators above it (a blank row, a date line) left out. Empty
/// (`start == end`) for a message that draws no row: a report folded
/// into the next one, a reaction under its message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    pub start: usize,
    pub end: usize,
}

impl Lines {
    /// Whether the message draws any row.
    pub fn drawn(&self) -> bool {
        self.start < self.end
    }

    /// Whether any of its rows is in `top..top + rows`.
    pub fn shown(&self, top: usize, rows: usize) -> bool {
        self.drawn() && self.start < top + rows && self.end > top
    }
}

/// Where a chat stands: the view's first row and the selected message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub top: usize,
    pub selected: usize,
}

/// A target that lays only a window of the chat (the M9, 1490) keeps the
/// view's first row by a message: `offset` rows below the first line of
/// the message `key` (negative inside the separators above it). A window
/// that moves keeps the line the reader was on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub key: u64,
    pub offset: isize,
}

/// A target's laid window of the open chat, handed to the model with a
/// scroll press ([`super::UiEvent::Lines`]): the key of each message in
/// it (`Message::activity`), its lines in the window's rows, all the
/// window's rows, the rows on screen and the view's first row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub keys: alloc::vec::Vec<u64>,
    pub lines: alloc::vec::Vec<Lines>,
    pub total: usize,
    pub rows: usize,
    pub top: usize,
}

impl Window {
    /// The anchor of row `top`: the first message whose lines reach past
    /// it. None in a window without a drawn message.
    pub fn anchor(&self, top: usize) -> Option<Anchor> {
        let at = self
            .lines
            .iter()
            .position(|l| l.drawn() && l.end > top)
            .or_else(|| self.lines.iter().rposition(Lines::drawn))?;
        Some(Anchor {
            key: self.keys[at],
            offset: top as isize - self.lines[at].start as isize,
        })
    }

    /// The row `anchor` stands for here, held to the window; none when its
    /// message is not in it.
    pub fn row_of(&self, anchor: Anchor) -> Option<usize> {
        let at = self.keys.iter().position(|&k| k == anchor.key)?;
        let row = self.lines[at].start as isize + anchor.offset;
        Some((row.max(0) as usize).min(self.total.saturating_sub(self.rows)))
    }
}

/// Whether the last line of `lines` is in `top..top + rows`: the read
/// rule (1529), [`leviculum_ui::read::end_shown`] in rows, which the
/// window's chat applies in pixels (1457).
pub fn last_line_shown(lines: Lines, top: usize, rows: usize) -> bool {
    leviculum_ui::read::end_shown(lines.start, lines.end, top, rows)
}

/// The rows a page moves: a screen less one line of overlap.
pub fn page_rows(rows: usize) -> usize {
    rows.saturating_sub(1).max(1)
}

/// `cmd` from `view` over the chat's `lines` (one per message, in order),
/// `total` rows laid and `rows` on screen. The rule is the module's.
pub fn step(lines: &[Lines], total: usize, rows: usize, view: View, cmd: ScrollCmd) -> View {
    let rows = rows.max(1);
    let bottom = total.saturating_sub(rows);
    let top = view.top.min(bottom);
    let (Some(first), Some(last)) = (
        lines.iter().position(Lines::drawn),
        lines.iter().rposition(Lines::drawn),
    ) else {
        return View { top, ..view };
    };
    let next = |at: usize| (at + 1..lines.len()).find(|&i| lines[i].drawn());
    let prev = |at: usize| (0..at).rev().find(|&i| lines[i].drawn());
    let shown_in = |top: usize| move |i: &usize| lines.get(*i).is_some_and(|l| l.shown(top, rows));
    let down = matches!(
        cmd,
        ScrollCmd::LineDown | ScrollCmd::PageDown | ScrollCmd::HalfPageDown
    );
    let mut selected = view.selected;
    if !shown_in(top)(&selected) {
        // Off screen: the press selects what the reader sees.
        let on_screen = match down {
            true => (0..lines.len()).find(shown_in(top)),
            false => (0..lines.len()).rev().find(shown_in(top)),
        };
        selected = on_screen.unwrap_or(match down {
            true => next(selected.min(lines.len())).unwrap_or(last),
            false => prev(selected.min(lines.len())).unwrap_or(first),
        });
        if matches!(cmd, ScrollCmd::LineDown | ScrollCmd::LineUp) {
            return View { top, selected };
        }
    }
    let page = |top: usize, step: usize, down: bool| {
        let to = match down {
            true => (top + step).min(bottom),
            false => top.saturating_sub(step),
        };
        // A page that reaches an end selects that end's message: the
        // newest at the bottom (the pane follows it again), the first at
        // the top.
        match (down, to) {
            (true, to) if to == bottom => {
                return View {
                    top: to,
                    selected: last,
                }
            }
            (false, 0) => {
                return View {
                    top: 0,
                    selected: first,
                }
            }
            _ => {}
        }
        // The messages the page brought lines of into view.
        let came = |i: &usize| {
            let l = lines[*i];
            l.shown(to, rows)
                && match down {
                    true => l.end > top + rows,
                    false => l.start < top,
                }
        };
        let kept = shown_in(to)(&selected);
        let selected = match down {
            true => match (0..lines.len()).find(came) {
                Some(c) if !(kept && selected >= c) => c,
                _ if kept => selected,
                _ => (0..lines.len()).find(shown_in(to)).unwrap_or(selected),
            },
            false => match (0..lines.len()).rev().find(came) {
                Some(c) if !(kept && selected <= c) => c,
                _ if kept => selected,
                _ => (0..lines.len())
                    .rev()
                    .find(shown_in(to))
                    .unwrap_or(selected),
            },
        };
        View { top: to, selected }
    };
    match cmd {
        ScrollCmd::LineDown => match next(selected) {
            _ if lines[selected].end > top + rows => View {
                top: top + 1,
                selected,
            },
            // Its first line on screen: selected, nothing moves.
            Some(n) if lines[n].start < top + rows => View { top, selected: n },
            // A separator first, or rows after the last message.
            _ => View {
                top: (top + 1).min(bottom),
                selected,
            },
        },
        ScrollCmd::LineUp => match prev(selected) {
            _ if lines[selected].start < top => View {
                top: top - 1,
                selected,
            },
            Some(p) if lines[p].end > top => View { top, selected: p },
            _ => View {
                top: top.saturating_sub(1),
                selected,
            },
        },
        ScrollCmd::PageDown => page(top, page_rows(rows), true),
        ScrollCmd::PageUp => page(top, page_rows(rows), false),
        ScrollCmd::HalfPageDown => page(top, (rows / 2).max(1), true),
        ScrollCmd::HalfPageUp => page(top, (rows / 2).max(1), false),
        ScrollCmd::Top => View {
            top: 0,
            selected: first,
        },
        ScrollCmd::Bottom => View {
            top: bottom,
            selected: last,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A short message, one forty lines long, a short one; a blank row
    /// between each two; a screen of ten.
    fn long_between_short() -> (Vec<Lines>, usize) {
        let lines = alloc::vec![
            Lines { start: 0, end: 2 },
            Lines { start: 3, end: 43 },
            Lines { start: 44, end: 46 },
        ];
        (lines, 46)
    }

    fn press(lines: &[Lines], total: usize, view: View, cmd: ScrollCmd) -> View {
        step(lines, total, 10, view, cmd)
    }

    /// Down from the top shows every row exactly once, one more a press
    /// while the long message scrolls, and the selection moves only when
    /// the message it is on has shown its last line.
    #[test]
    fn down_shows_every_line_once_and_the_selection_follows_the_lines() {
        let (lines, total) = long_between_short();
        let mut view = View {
            top: 0,
            selected: 0,
        };
        let mut seen = alloc::vec![0usize; total];
        seen[..10].iter_mut().for_each(|n| *n += 1);
        let mut selections = Vec::new();
        for _ in 0..60 {
            let before = view.top;
            view = press(&lines, total, view, ScrollCmd::LineDown);
            assert!(view.top - before <= 1, "never more than a line: {view:?}");
            seen[before + 10..view.top + 10]
                .iter_mut()
                .for_each(|n| *n += 1);
            selections.push(view.selected);
        }
        assert!(seen.iter().all(|&n| n == 1), "{seen:?}");
        // The first press selects the long one (the short one is whole,
        // the long one's first line on screen); it stays selected until
        // its last row (42) is on screen, row 33 at the top.
        // The press after its last line came on, and the two the blank
        // row and the short one's first line took, select the short one.
        assert_eq!(selections[0], 1);
        let moved = selections.iter().position(|&s| s == 2).expect("moved on");
        assert_eq!(moved, 36, "{selections:?}");
        assert!(selections[1..moved].iter().all(|&s| s == 1));
    }

    /// Back up from the newest: the long message is entered at its end and
    /// read line by line, not at its start.
    #[test]
    fn up_enters_a_long_message_at_the_line_left() {
        let (lines, total) = long_between_short();
        let mut view = View {
            top: 36,
            selected: 2,
        };
        view = press(&lines, total, view, ScrollCmd::LineUp);
        assert_eq!(
            view,
            View {
                top: 36,
                selected: 1
            },
            "the long one, its end on screen"
        );
        view = press(&lines, total, view, ScrollCmd::LineUp);
        assert_eq!(
            view,
            View {
                top: 35,
                selected: 1
            },
            "a line up into it, not its start"
        );
    }

    /// Down to the middle of the long message and back up: the way back
    /// lands where it left, a line a press.
    #[test]
    fn down_and_back_up_keeps_the_place_in_the_long_message() {
        let (lines, total) = long_between_short();
        let mut view = View {
            top: 0,
            selected: 0,
        };
        for _ in 0..21 {
            view = press(&lines, total, view, ScrollCmd::LineDown);
        }
        assert_eq!(
            view,
            View {
                top: 20,
                selected: 1
            }
        );
        view = press(&lines, total, view, ScrollCmd::LineUp);
        assert_eq!(
            view,
            View {
                top: 19,
                selected: 1
            }
        );
    }

    /// A page moves a screen less a line, onto the first message it
    /// brought lines of; at the end it selects the end.
    #[test]
    fn a_page_moves_a_screen_less_a_line() {
        let (lines, total) = long_between_short();
        let start = View {
            top: 0,
            selected: 0,
        };
        let view = press(&lines, total, start, ScrollCmd::PageDown);
        assert_eq!(
            view,
            View {
                top: 9,
                selected: 1
            }
        );
        let view = press(&lines, total, view, ScrollCmd::PageDown);
        assert_eq!(
            view,
            View {
                top: 18,
                selected: 1
            },
            "still in the long one"
        );
        let view = press(&lines, total, view, ScrollCmd::PageUp);
        assert_eq!(
            view,
            View {
                top: 9,
                selected: 1
            },
            "the long one brought lines back"
        );
        let view = press(&lines, total, view, ScrollCmd::PageUp);
        assert_eq!(
            view,
            View {
                top: 0,
                selected: 0
            },
            "the top selects the first"
        );
        let at_end = View {
            top: 36,
            selected: 1,
        };
        assert_eq!(
            press(&lines, total, at_end, ScrollCmd::PageDown),
            View {
                top: 36,
                selected: 2
            }
        );
        assert_eq!(
            press(&lines, total, start, ScrollCmd::PageUp),
            View {
                top: 0,
                selected: 0
            }
        );
    }

    /// Short messages that all fit move the selection a message a press,
    /// as before (the positive control of the line rule: nothing scrolls).
    #[test]
    fn short_messages_on_one_screen_step_by_message() {
        let lines = [
            Lines { start: 0, end: 1 },
            Lines { start: 2, end: 3 },
            Lines { start: 4, end: 5 },
        ];
        let mut view = View {
            top: 0,
            selected: 0,
        };
        for want in [1, 2, 2] {
            view = step(&lines, 5, 10, view, ScrollCmd::LineDown);
            assert_eq!(
                view,
                View {
                    top: 0,
                    selected: want
                }
            );
        }
    }

    /// A selection scrolled off screen is not walked back to.
    #[test]
    fn a_selection_off_screen_is_replaced_by_what_shows() {
        let (lines, total) = long_between_short();
        let view = View {
            top: 20,
            selected: 0,
        };
        assert_eq!(
            press(&lines, total, view, ScrollCmd::LineDown),
            View {
                top: 20,
                selected: 1
            }
        );
    }

    /// The read rule is the last line, not the first.
    #[test]
    fn the_last_line_on_screen_reads() {
        let long = Lines { start: 3, end: 43 };
        assert!(!last_line_shown(long, 0, 10));
        assert!(!last_line_shown(long, 32, 10));
        assert!(last_line_shown(long, 33, 10));
        assert!(!last_line_shown(Lines::default(), 0, 10));
    }

    /// A window's anchor names the row again after the window moved.
    #[test]
    fn an_anchor_survives_a_moved_window() {
        let window = Window {
            keys: alloc::vec![10, 11, 12],
            lines: alloc::vec![
                Lines { start: 0, end: 2 },
                Lines { start: 3, end: 43 },
                Lines { start: 44, end: 46 },
            ],
            total: 46,
            rows: 10,
            top: 0,
        };
        let anchor = window.anchor(20).expect("an anchor");
        assert_eq!(
            anchor,
            Anchor {
                key: 11,
                offset: 17
            }
        );
        let moved = Window {
            keys: alloc::vec![11, 12],
            lines: alloc::vec![Lines { start: 0, end: 40 }, Lines { start: 41, end: 43 }],
            total: 43,
            ..window.clone()
        };
        assert_eq!(moved.row_of(anchor), Some(17));
        assert_eq!(window.row_of(Anchor { key: 99, offset: 0 }), None);
    }
}
