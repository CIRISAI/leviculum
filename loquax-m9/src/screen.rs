//! The scene as styled lines on the M9's cell grid: 35 x 16 cells of the
//! 9 x 15 atlas or 32 x 12 of the 10 x 20, the reader's [`Size`]
//! ([`crate::font`]), on the 320 x 240 panel. The chats list here,
//! the open chat under its bar in [`crate::chat`].
//!
//! The scene is the one the phone and the window draw (`Scene`,
//! `loquax::ui::scene`); this words and places it in cells, as the
//! terminal's list does (`loquax::ui::layout::list`): two rows a
//! conversation, the name and the time over the last line and its count.
//! Its own line is the top bar above, which the terminal draws as a pane
//! border. The tab bar below is drawn only for a scene that carries one;
//! the M9's does not (1532, [`viewing`]).
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
use loquax::when::WallTime;

/// Cells across and rows of the large size's grid ([`crate::font`]): the
/// map's; every other screen takes its grid from the reader's [`Size`].
pub use crate::font::{Size, COLS, ROWS};

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

/// What the board's loop drew last (1491): a turn whose pull brought
/// nothing, or nothing new, draws nothing. The loop asks [`Drawn::due`]
/// after it folded what the client drained, and says [`Drawn::drawn`]
/// at the end of every turn: every other path of a turn that changes the
/// model (a key, a control, the pairing) draws it there and then.
#[derive(Clone, Copy, Debug, Default)]
pub struct Drawn {
    presented: loquax::ui::Presented,
}

impl Drawn {
    /// Whether the turn draws: the model's revision moved since the last
    /// frame.
    pub fn due<B>(&mut self, model: &UiModel<B>) -> bool {
        self.presented.due(Self::stamp(model))
    }

    /// The frame on the panel shows `model` as it is.
    pub fn drawn<B>(&mut self, model: &UiModel<B>) {
        self.presented.drawn(Self::stamp(model));
    }

    /// How many draws [`Drawn::due`] allowed.
    pub fn builds(&self) -> u64 {
        self.presented.builds()
    }

    fn stamp<B>(model: &UiModel<B>) -> u64 {
        loquax::ui::Presented::stamp(model.revision, &[])
    }
}

/// What the M9's screen shows in `size` (1490): the list's entries under
/// the top bar, and in a chat at most one bubble row a cell row. The
/// scene is built for this window and its margin, never for the whole
/// store ([`loquax::ui::Viewing`]). No tab bar (1532): the M9 has a key
/// for the map and one for the house, and the bar's row is the content's.
pub fn viewing(size: Size) -> loquax::ui::Viewing {
    let below = size.grid().1 - 1;
    loquax::ui::Viewing {
        list_rows: below / ENTRY_ROWS,
        chat_rows: below,
        apart: true,
        tabs: false,
    }
}

/// The size `model` is viewed in: the one whose [`viewing`] the board
/// set on it (1532), [`Size::DEFAULT`] for a model no board set.
pub fn size_of<B>(model: &UiModel<B>) -> Size {
    Size::ALL
        .into_iter()
        .find(|&size| viewing(size) == model.viewing)
        .unwrap_or(Size::DEFAULT)
}

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
    /// Where the chat's view stands when the reader scrolled it by the
    /// line (1529, the model's `chat_anchor`); none draws the newest at
    /// the bottom, or the cursor's bubble there when it stands above.
    pub anchor: Option<loquax::ui::pager::Anchor>,
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
    /// An error that is a state, not a log line (1493): a write that did
    /// not reach the flash, a join the record could not keep. It takes
    /// the home word's place at the bar's right end until it clears.
    pub fault: Option<loquax::ui::strings::StrId>,
    /// What the map shows over its picture (1531): the key hint and the
    /// corner credit, which the board's clock says ([`crate::map::Chrome`]).
    pub map: crate::map::Overlay,
}

impl Look {
    /// The M9's: truecolor on dark, the reader's language, no cursor.
    pub fn m9(lang: Lang, now: Option<WallTime>) -> Self {
        Self {
            tokens: &leviculum_ui::tokens::TRUECOLOR_DARK,
            lang,
            now,
            cursor: None,
            anchor: None,
            caret: None,
            row: 0,
            home: None,
            fault: None,
            map: crate::map::Overlay {
                hint: None,
                credit: false,
                sky: crate::gnss::Sky::Off,
            },
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
            anchor: model.chat_anchor.filter(|_| model.focus == Pane::Messages),
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
        // The list alone, no chat built for it (1463).
        Pane::Conversations => {
            let mut scene = model.list_scene(LayoutClass::Wide);
            scene.screen = ScreenId::Chats;
            scene.in_class(LayoutClass::Compact)
        }
        _ => model.scene(LayoutClass::Compact),
    }
}

/// Where the drawn screen stands in what it shows (1533): the list's
/// selected row and first row on screen, as places in the whole order,
/// and the open chat's topmost bubble on screen by its key. What the
/// board's `[PAGE]` line says ([`crate::page`]); none of it is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// The list's `(selected, first on screen)`, while the list is drawn.
    pub list: Option<(usize, usize)>,
    /// The key of the chat's topmost bubble on screen, while a chat is.
    pub chat_top: Option<u64>,
}

/// The screen the scene is, `cols` x `rows` cells: the top bar, the list
/// scrolled to show its selected row, the tab bar; or, with a chat open
/// and no list, the chat's bar over the chat ([`crate::chat`]).
pub fn lines(scene: &Scene, look: &Look, cols: usize, rows: usize) -> Vec<RLine> {
    lines_at(scene, look, cols, rows).0
}

/// [`lines`], and where the screen stands ([`Window`]).
pub fn lines_at(scene: &Scene, look: &Look, cols: usize, rows: usize) -> (Vec<RLine>, Window) {
    let (lines, window, _) = lines_seen(scene, look, cols, rows);
    (lines, window)
}

/// [`lines_at`], and the keys of the open chat's bubbles whose last line
/// is on screen (1541, [`crate::read`]); none while no chat is drawn: the
/// list, a group's sheet over the chat.
pub fn lines_seen(
    scene: &Scene,
    look: &Look,
    cols: usize,
    rows: usize,
) -> (Vec<RLine>, Window, Option<Vec<u64>>) {
    let mut window = Window::default();
    let mut seen = None;
    if let Some(sheet) = crate::group::lines(scene, look, cols, rows) {
        return (sheet, window, seen);
    }
    let mut out = Vec::with_capacity(rows);
    // Two peers of one name (1587): 35 cells cut the bar's sentence to
    // "Lew ·1e1e · Zwei Kontakte he…", so in a chat the bar says the name
    // and the sentence takes a row of its own under it, whole; without a
    // collision the chat keeps every row.
    let same = match (&scene.list, &scene.chat) {
        (None, Some(chat)) => chat.same_name.as_ref(),
        _ => None,
    };
    let title = match (same, &scene.top.title) {
        (Some(sentence), Text::Join(parts)) if parts.last() == Some(sentence) => {
            match &parts[..parts.len() - 1] {
                [only] => only.clone(),
                rest => Text::Join(rest.to_vec()),
            }
        }
        (_, title) => title.clone(),
    };
    out.push(top_bar(&scene.top, &title, look, cols));
    if let Some(sentence) = same {
        let words = cut(&sentence.resolve(look.lang), cols.saturating_sub(1));
        out.push(line(
            &[
                (" ", RStyle::default()),
                (
                    &words,
                    RStyle {
                        italic: true,
                        ..muted(look)
                    },
                ),
            ],
            cols,
        ));
    }
    // The bar, and the sentence's row while there is one.
    let head = out.len();
    let below = rows.saturating_sub(head + usize::from(scene.tabs.is_some()));
    if let (None, Some(chat)) = (&scene.list, &scene.chat) {
        let (chat_lines, top, keys) = crate::chat::lines_at(chat, look, cols, below);
        out.extend(chat_lines);
        window.chat_top = top;
        seen = Some(keys);
    }
    if let Some(list) = &scene.list {
        match list.rows.is_empty() {
            true => {
                window.list = Some((0, 0));
                let words = list.empty.resolve(look.lang);
                out.push(line(&[(&cut(&words, cols), muted(look))], cols));
            }
            false => {
                let shown = (below / ENTRY_ROWS).max(1);
                let at = list.rows.iter().position(|r| r.selected).unwrap_or(0);
                // The core's line for the cursor (1546): its row keeps it
                // while a pull reorders the rows around it. Kept on the
                // screen whatever the scene was built for.
                let lowest = (at + 1).saturating_sub(shown);
                let first = match list.top {
                    Some(top) => top.saturating_sub(list.first).clamp(lowest, at),
                    None => lowest,
                };
                window.list = Some((list.first + at, list.first + first));
                for row in list.rows.iter().skip(first).take(shown) {
                    out.extend(entry(row, look, cols));
                }
            }
        }
    }
    out.truncate(head + below);
    while out.len() < head + below {
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
    (out, window, seen)
}

/// The word at the top bar's right end (1564): a fault, else the home's
/// state (1561 took the type switch's note out with the held house key), each one short word
/// that stands on its own ([`Word::bar`](loquax::home::board::Word::bar)).
/// None on a board that is no device of a home and has nothing to say.
pub fn bar_word(look: &Look) -> Option<alloc::string::String> {
    use loquax::ui::strings::{StrId, Text as Words};
    if let Some(fault) = look.fault {
        // A write that did not land matters more than the fetch (1493);
        // its sentence is the log's, the bar says the state.
        let id = match fault {
            StrId::M9NotSaved | StrId::M9JoinNotSaved => StrId::M9BarNotSaved,
            other => other,
        };
        return Some(Words::key(id).resolve(look.lang));
    }
    look.home.map(|word| word.bar().resolve(look.lang))
}

/// The top bar (1564): in a chat the peer's avatar, then `title`, the
/// page's or the chat's name, and at the right end at most one short word
/// of state ([`bar_word`]). The list's bar has no avatar: the reader's
/// own, a sign with no name behind it on the M9 ("?", "90"), read as a
/// number. The word stands whole or not at all: where it and the whole
/// title do not both fit, the title has the bar, cut as a name is cut.
fn top_bar(top: &TopBar, title: &Text, look: &Look, cols: usize) -> RLine {
    let chrome = painted(look.tokens.chrome);
    let mut bar = RLine::default();
    if let Lead::Back { avatar, .. } = &top.lead {
        push(&mut bar, " ", chrome);
        push_avatar(&mut bar, avatar);
    }
    let title = title.resolve(look.lang);
    // A cell before the title, one closing the bar.
    let room = cols.saturating_sub(bar.cells.len() + 2);
    let word = bar_word(look).unwrap_or_default();
    let words = fit_width(&word);
    let whole = fit_width(&narrow(&title));
    let word = match words > 0 && whole + 1 + words <= room {
        true => word,
        false => alloc::string::String::new(),
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
    if !word.is_empty() {
        let gap = cols.saturating_sub(bar.cells.len() + words + 1);
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
        (Some(at), Some(now)) => loquax::ui::scene::list_time(&at, &now).resolve(look.lang),
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
    // Two peers of one name (1452, 1587): the row keeps the name with its
    // tail alone; the open chat says the sentence on its own row.
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
                mark_glyph(row),
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
        Mark::Arrived => DeliveryState::Arrived,
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

/// A delivery mark as the board draws it (1447, walk 1 finding 23): the
/// ladder's own glyphs, which the 10 x 20 atlas has (◷, ✓, the braille
/// spinner, !), but ✔, which it lacks: read is ✓✓ in the read tone and
/// bold, as on every other target the tone tells it from delivered. Never
/// the ASCII rung's "v" and "VV".
pub(crate) fn mark_glyph(row: &leviculum_ui::delivery::DeliveryGlyph) -> String {
    row.symbol(0, false).trim_end().replace('✔', "✓")
}

/// `text` [`narrow`]ed and cut to `cols` cells, an ellipsis where it was cut.
pub(crate) fn cut(text: &str, cols: usize) -> String {
    truncate_to_cols(&narrow(text), cols)
}

/// `line` cut or padded to `cols` cells, the padding in `style`, one cell
/// a character as [`narrow`] makes them.
pub fn fill(line: RLine, cols: usize, style: RStyle) -> RLine {
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

pub fn push(line: &mut RLine, text: &str, st: RStyle) {
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
        assert_eq!(rungs, [". ", ". ", "v ", "v ", "vv", "VV", "! "]);
        // The board draws the glyphs, the atlas's (1447).
        let drawn: Vec<String> = MARKS
            .iter()
            .map(|&m| mark_glyph(glyph(state_of(m))))
            .collect();
        assert_eq!(drawn, ["◷", "◷", "✓", "✓", "✓✓", "✓✓", "!"]);
        // ✔ is the atlas's missing glyph, the control: whatever it maps
        // to, every drawn mark maps elsewhere.
        let atlas = crate::font::atlas();
        let missing = atlas.glyph_mapping.index('✔');
        assert_eq!(
            missing,
            atlas.glyph_mapping.index('\u{e000}'),
            "the control"
        );
        for d in &drawn {
            for ch in d.chars() {
                assert_ne!(
                    atlas.glyph_mapping.index(ch),
                    missing,
                    "{ch} is in the atlas"
                );
            }
        }
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
