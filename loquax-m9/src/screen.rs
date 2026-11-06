//! The scene as styled lines on the M9's cell grid: 32 x 12 cells of the
//! 10 x 20 atlas ([`crate::font`]) on the 320 x 240 panel. The chats list here,
//! the open chat under its bar in [`crate::chat`].
//!
//! The scene is the one the phone and the window draw (`Scene`,
//! `loquax::ui::scene`); this words and places it in cells, as the
//! terminal's list does (`loquax::ui::layout::list`): two rows a
//! conversation, the name and the time over the last line and its count.
//! Its own lines are the top bar above and the tab bar below, which the
//! terminal draws as pane borders and the M9 has no room for.
//!
//! The delivery marks stand on their ASCII ladder
//! ([`leviculum_ui::delivery::DeliveryGlyph::ascii`]): the atlas has no
//! U+2713, U+2714 or U+25F7 (`handheld-messenger.md`, "The renderer,
//! measured"), so the tick ladder would not render.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::color::Ink;
use leviculum_ui::delivery::{glyph, DeliveryState};
use leviculum_ui::input::InputBuffer;
use leviculum_ui::style::{RLine, RStyle, StyledChar};
use leviculum_ui::text::{fit_width, truncate_to_cols};
use leviculum_ui::tokens::{Paint, Tokens};
use loquax::ui::chat::Mark;
use loquax::ui::scene::{Avatar, LayoutClass, Lead, Row, Scene, ScreenId, TopBar};
use loquax::ui::strings::{Lang, StrId, Text};
use loquax::ui::{Pane, UiModel};
use loquax::when::{list_time, WallTime};

/// Cells across and rows, the atlas's grid ([`crate::font`]).
pub use crate::font::{COLS, ROWS};

/// Below this many cells a grid is narrow (1428), the M9's 32: the top
/// bar gives the home's word the room its title does not use, the title
/// a third at most while a word stands there, and the word runs to the
/// bar's last cell. The terminal's own narrow class starts at the same
/// width (`loquax::ui::layout::NARROW`), where its bubbles take the
/// whole pane, which the M9's chat does through `block_cap`.
pub const NARROW_COLS: usize = loquax::ui::layout::NARROW;

/// Rows a conversation takes: the name and time, the last line and its
/// count or mark.
pub const ENTRY_ROWS: usize = 2;

/// The selection's bar in the margin, the terminal's (`layout::BAR`).
pub const BAR: char = '▌';

/// Cells of an avatar: its initials, two cells, with a cell either side.
const AVATAR: usize = 4;

/// What drawing a scene needs besides the scene.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    /// The colour tokens; the M9 draws in truecolor.
    pub tokens: &'static Tokens,
    pub lang: Lang,
    /// The time of day, for the list's times; none draws none.
    pub now: Option<WallTime>,
    /// The open chat's cursor, the key of the bubble it stands on
    /// (`BubbleNode::bubble.key`), while the chat has the keys; none draws
    /// no bubble selected and the newest at the bottom.
    pub cursor: Option<u64>,
    /// The compose field's cursor, in characters, while it has the keys;
    /// none draws no caret, and the hint on an empty field.
    pub caret: Option<usize>,
    /// The M9's own cursor over a group's sheet or picker, a row index
    /// ([`crate::group`]); the model has none for them.
    pub row: usize,
    /// What the top bar says of the board's home at its right end (1415):
    /// joining, fetched N of M, unreachable. None on a board that is no
    /// device of a home, which draws the bar as before.
    pub home: Option<loquax::home::board::Word>,
}

impl Look {
    /// The M9's: truecolor on dark, the reader's language, no cursor.
    pub fn m9(lang: Lang, now: Option<WallTime>) -> Self {
        Self {
            tokens: &leviculum_ui::tokens::TRUECOLOR_DARK,
            lang,
            now,
            cursor: None,
            caret: None,
            row: 0,
            home: None,
        }
    }

    /// The M9's look of `model`: its language and time, and where its keys
    /// go, the chat's cursor or the compose field's.
    pub fn of<B: InputBuffer>(model: &UiModel<B>) -> Self {
        let cursor = match model.focus {
            Pane::Messages => model
                .conversations
                .get(model.selected)
                .and_then(|c| c.messages.get(model.message_cursor))
                .map(|m| m.activity),
            _ => None,
        };
        Self {
            cursor,
            caret: (model.focus == Pane::Compose).then(|| model.compose.cursor()),
            ..Self::m9(model.lang, model.now)
        }
    }
}

/// The scene the M9 draws of `model`: the Compact one, which is the open
/// chat alone, and the list while the keys are on the list. The model's
/// back from the chat moves the keys to the list and keeps the chat open,
/// as the terminal's panes do; a screen of one pane shows the pane the keys
/// are in.
pub fn scene<B>(model: &UiModel<B>) -> Scene {
    match model.focus {
        Pane::Conversations => {
            let mut scene = model.scene(LayoutClass::Wide);
            scene.chat = None;
            scene.sheet = None;
            scene.whereabouts = None;
            scene.screen = ScreenId::Chats;
            scene.in_class(LayoutClass::Compact)
        }
        _ => model.scene(LayoutClass::Compact),
    }
}

/// The screen the scene is, `cols` x `rows` cells: the top bar, the list
/// scrolled to show its selected row, the tab bar; or, with a chat open
/// and no list, the chat's bar over the chat ([`crate::chat`]).
pub fn lines(scene: &Scene, look: &Look, cols: usize, rows: usize) -> Vec<RLine> {
    if let Some(sheet) = crate::group::lines(scene, look, cols, rows) {
        return sheet;
    }
    let mut out = Vec::with_capacity(rows);
    out.push(top_bar(&scene.top, look, cols));
    let below = rows.saturating_sub(1 + usize::from(scene.tabs.is_some()));
    if let (None, Some(chat)) = (&scene.list, &scene.chat) {
        out.extend(crate::chat::lines(chat, look, cols, below));
    }
    if let Some(list) = &scene.list {
        match list.rows.is_empty() {
            true => {
                let words = list.empty.resolve(look.lang);
                out.push(line(&[(&cut(&words, cols), muted(look))], cols));
            }
            false => {
                let shown = (below / ENTRY_ROWS).max(1);
                let at = list.rows.iter().position(|r| r.selected).unwrap_or(0);
                let first = (at + 1).saturating_sub(shown);
                for row in list.rows.iter().skip(first).take(shown) {
                    out.extend(entry(row, look, cols));
                }
            }
        }
    }
    out.truncate(1 + below);
    while out.len() < 1 + below {
        out.push(line(&[], cols));
    }
    if let Some(tabs) = &scene.tabs {
        let mut bar = RLine::default();
        for tab in &tabs.items {
            let label = narrow(&tab.label.resolve(look.lang));
            let words = match tab.badge {
                Some(n) if n > 0 => format!(" {label} {n} "),
                _ => format!(" {label} "),
            };
            let paint = match tab.id == tabs.active {
                true => look.tokens.selection,
                false => look.tokens.chrome,
            };
            push(&mut bar, &words, painted(paint));
        }
        out.push(fill(bar, cols, painted(look.tokens.chrome)));
    }
    out.truncate(rows);
    out
}

/// The top bar: the reader's avatar (or the chat's), the title.
fn top_bar(top: &TopBar, look: &Look, cols: usize) -> RLine {
    let chrome = painted(look.tokens.chrome);
    let avatar = match &top.lead {
        Lead::Me { avatar, .. } | Lead::Back { avatar, .. } => avatar,
    };
    let mut bar = RLine::default();
    push(&mut bar, " ", chrome);
    push_avatar(&mut bar, avatar);
    let title = top.title.resolve(look.lang);
    let room = cols.saturating_sub(AVATAR + 2);
    let tight = cols < NARROW_COLS;
    // The home's word at the right end, the title cut before it. Wide,
    // the title keeps a third of the bar at least and a cell closes the
    // bar; narrow, the title takes its own width up to a third and the
    // word the rest, to the last cell.
    let budget = match tight {
        false => (room * 2) / 3,
        true => room.saturating_sub(fit_width(&narrow(&title)).min(room / 3) + 1),
    };
    let word = look
        .home
        .map(|word| cut(&word.text().resolve(look.lang), budget))
        .unwrap_or_default();
    let words = word.chars().count();
    let room = match (words, tight) {
        (0, _) => room,
        (n, false) => room.saturating_sub(n + 2),
        (n, true) => room.saturating_sub(n + 1),
    };
    let title = cut(&title, room);
    push(
        &mut bar,
        &format!(" {title}"),
        RStyle {
            bold: true,
            ..chrome
        },
    );
    if words > 0 {
        let gap = cols.saturating_sub(bar.cells.len() + words + usize::from(!tight));
        push(&mut bar, &" ".repeat(gap), chrome);
        push(&mut bar, &word, chrome);
    }
    fill(bar, cols, chrome)
}

/// One conversation's two rows, `cols` cells each: the bar's margin, the
/// avatar, the name and the time over the last line and its count, or our
/// last message's mark. The selected one behind the bar, reversed.
fn entry(row: &Row, look: &Look, cols: usize) -> [RLine; 2] {
    let shown = &row.row;
    let time = match (shown.at, look.now) {
        (Some(at), Some(now)) => list_time(&at, &now),
        _ => String::new(),
    };
    // The bar, the avatar, a gap; a cell at the right end.
    let room = cols.saturating_sub(1 + AVATAR + 1 + 1);

    let mut top = RLine::default();
    push(&mut top, " ", RStyle::default());
    push_avatar(&mut top, &row.avatar);
    push(&mut top, " ", RStyle::default());
    let name = cut(&shown.name, room.saturating_sub(fit_width(&time) + 1));
    push(
        &mut top,
        &name,
        RStyle {
            bold: true,
            ..RStyle::default()
        },
    );
    let gap = room.saturating_sub(fit_width(&name) + fit_width(&time));
    push(&mut top, &" ".repeat(gap), RStyle::default());
    push(&mut top, &time, muted(look));

    // The right end of the second row: the unread count, else our mark.
    let (end, end_style) = match (shown.unread, shown.mark) {
        (n, _) if n > 0 => (
            format!("[{n}]"),
            RStyle {
                reverse: true,
                ..painted(look.tokens.unread)
            },
        ),
        (_, Some(mark)) => {
            let row = glyph(state_of(mark));
            (
                String::from(row.ascii.trim_end()),
                RStyle {
                    fg: look.tokens.tones.of(row.tone),
                    bold: row.bold,
                    ..RStyle::default()
                },
            )
        }
        _ => (String::new(), RStyle::default()),
    };
    let (words, italic) = match (&row.draft, &row.live) {
        (Some(draft), _) => (draft.resolve(look.lang), true),
        (None, Some(live)) => (live.resolve(look.lang), false),
        (None, None) if shown.ours && !shown.last.is_empty() => (
            format!(
                "{}: {}",
                Text::key(StrId::QuoteYou).resolve(look.lang),
                shown.last
            ),
            false,
        ),
        // A group with no line yet says how many are in it, or that the
        // reader is invited (`GroupBadge::words`).
        (None, None) if shown.last.is_empty() && row.group.is_some() => (
            row.group
                .as_ref()
                .map(|g| g.words.resolve(look.lang))
                .unwrap_or_default(),
            false,
        ),
        (None, None) => (shown.last.clone(), false),
    };
    let budget = match fit_width(&end) {
        0 => room,
        n => room.saturating_sub(n + 1),
    };
    let words = cut(&words, budget);
    let mut bottom = RLine::default();
    push(&mut bottom, &" ".repeat(1 + AVATAR + 1), RStyle::default());
    push(
        &mut bottom,
        &words,
        RStyle {
            italic,
            ..muted(look)
        },
    );
    let gap = room.saturating_sub(fit_width(&words) + fit_width(&end));
    push(&mut bottom, &" ".repeat(gap), RStyle::default());
    push(&mut bottom, &end, end_style);

    let mut rows = [
        fill(top, cols, RStyle::default()),
        fill(bottom, cols, RStyle::default()),
    ];
    if row.selected {
        let selection = painted(look.tokens.selection);
        for line in &mut rows {
            if let Some(cell) = line.cells.first_mut() {
                cell.ch = BAR;
            }
            for cell in line.cells.iter_mut().skip(1) {
                cell.st.fg = selection.fg.or(cell.st.fg);
                cell.st.bg = selection.bg.or(cell.st.bg);
                cell.st.reverse = selection.reverse;
            }
        }
    }
    rows
}

/// The delivery state a scene mark stands for, so the shared glyph table
/// words it: the list's mark folds the queue's three states and both
/// failures into one each (`chat::Mark::of`), and every state it folds
/// shares that mark's ASCII.
pub fn state_of(mark: Mark) -> DeliveryState {
    match mark {
        Mark::Sending => DeliveryState::Queued,
        Mark::Unconfirmed => DeliveryState::Sent,
        Mark::Mailbox => DeliveryState::InMailbox,
        Mark::Delivered => DeliveryState::Delivered,
        Mark::Read => DeliveryState::Read { at: 0 },
        Mark::Failed => DeliveryState::Failed,
    }
}

/// An avatar: its initials in its colours, a cell either side.
pub(crate) fn push_avatar(line: &mut RLine, avatar: &Avatar) {
    let style = RStyle {
        fg: Some(rgb(avatar.fg)),
        bg: Some(rgb(avatar.bg)),
        bold: true,
        ..RStyle::default()
    };
    let initials = cut(&avatar.initials, AVATAR - 2);
    let pad = (AVATAR - 2).saturating_sub(fit_width(&initials));
    push(line, &format!(" {initials}{} ", " ".repeat(pad)), style);
}

fn rgb(c: u32) -> Ink {
    Ink::Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub(crate) fn muted(look: &Look) -> RStyle {
    painted(look.tokens.muted)
}

/// A style drawn in a token's colours (`layout::painted`).
pub(crate) fn painted(paint: Paint) -> RStyle {
    loquax::ui::layout::painted(paint)
}

/// A line of runs, filled to `cols`.
fn line(runs: &[(&str, RStyle)], cols: usize) -> RLine {
    let mut out = RLine::default();
    for (text, style) in runs {
        push(&mut out, text, *style);
    }
    fill(out, cols, RStyle::default())
}

/// What a character the atlas cannot set in one cell is drawn as.
pub const NARROW: char = '?';

/// `text` one cell a character, as the panel draws it: a wide character
/// (an emoji, a CJK character) becomes [`NARROW`] and a zero-width one (a
/// joiner, a variation selector) goes. The atlas has no glyph wider than a
/// cell, and ratatui never sends a wide character's second cell, so a
/// redraw would leave the old pixels there; and a width measured before
/// this would put the right ends of a row out of line.
pub fn narrow(text: &str) -> String {
    text.chars()
        .filter_map(|ch| match fit_width(ch.encode_utf8(&mut [0; 4])) {
            0 => None,
            1 => Some(ch),
            _ => Some(NARROW),
        })
        .collect()
}

/// `text` [`narrow`]ed and cut to `cols` cells, an ellipsis where it was cut.
pub(crate) fn cut(text: &str, cols: usize) -> String {
    truncate_to_cols(&narrow(text), cols)
}

/// `line` cut or padded to `cols` cells, the padding in `style`, one cell
/// a character as [`narrow`] makes them.
pub(crate) fn fill(line: RLine, cols: usize, style: RStyle) -> RLine {
    let mut out = RLine::default();
    for mut cell in line.cells {
        match fit_width(cell.ch.encode_utf8(&mut [0; 4])) {
            0 => continue,
            1 => {}
            _ => cell.ch = NARROW,
        }
        if out.cells.len() == cols {
            break;
        }
        out.cells.push(cell);
    }
    let pad = cols - out.cells.len();
    push(&mut out, &" ".repeat(pad), style);
    out
}

pub(crate) fn push(line: &mut RLine, text: &str, st: RStyle) {
    line.cells.extend(text.chars().map(|ch| StyledChar {
        ch,
        st,
        link: None,
        field: None,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use loquax::ui::chat::MARKS;

    #[test]
    fn every_mark_has_an_ascii_rung_the_atlas_draws() {
        let rungs: Vec<&str> = MARKS.iter().map(|&m| glyph(state_of(m)).ascii).collect();
        assert_eq!(rungs, [". ", ". ", "v ", "vv", "VV", "! "]);
        for (mark, rung) in MARKS.iter().zip(&rungs) {
            assert!(rung.is_ascii(), "{mark:?} is {rung:?}");
            assert_eq!(
                Mark::of(Some(state_of(*mark))),
                *mark,
                "{mark:?} round trips"
            );
        }
    }

    #[test]
    fn a_line_is_filled_and_cut_to_the_grid() {
        let short = line(&[("ab", RStyle::default())], 5);
        assert_eq!(short.cells.len(), 5);
        let long = line(&[(&"x".repeat(60), RStyle::default())], COLS);
        assert_eq!(long.cells.len(), COLS);
        // A wide character takes one cell as the atlas's stand-in, a
        // joiner none.
        let wide = line(&[("a漢\u{200d}b", RStyle::default())], 5);
        let text: String = wide.cells.iter().map(|c| c.ch).collect();
        assert_eq!(text, "a?b  ");
    }
}
