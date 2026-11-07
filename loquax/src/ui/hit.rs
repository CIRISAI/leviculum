//! What is drawn at a screen cell: the mouse's half of the layout
//! (`docs/src/concepts/loquax-keys-and-layout.md`, section 5, "What the mouse
//! does").
//!
//! Every answer comes from what the screen draws: the [`Screen`]'s
//! rectangles the renderer draws in, the [`super::view`] it draws, and the
//! conversation laid out by [`super::layout`]. A click lands on the cells the
//! reader sees, and a test asks this module, not the frame, which cell is
//! which target.
//!
//! The two targets that transmit are words: `send` on the compose box's
//! border and the mailbox state in the header. Their hit boxes are the
//! word's cells, never a glyph beside them.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::footer;
use leviculum_ui::geometry::rect_contains;
use leviculum_ui::input::InputBuffer;
use leviculum_ui::Rect;

use super::layout::{self, Laid, RowText, ENTRY_ROWS};
use super::search;
use super::{
    detail_layout, palette_rect, view, FooterAct, Pane, Screen, Selection, UiModel, WIDE_HEADER,
};

/// What a cell is, to a click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Nothing: a cell no target covers, outside every pane.
    Nothing,
    /// A pane's border or its empty part: a click focuses the pane.
    Pane(Pane),
    /// A conversation's two rows in the list, by its index.
    Conversation(usize),
    /// A conversation's unread badge.
    Badge(usize),
    /// `/ search` on the list's bottom border: opens the search.
    SearchHint,
    /// The list's title, the reader's own name and its dot (1443): opens
    /// the profile, as a tap on the avatar does on the phone.
    Me,
    /// The open search's field and the blank under it: the cursor goes
    /// there.
    SearchField,
    /// A result of the open search, by its index among the results: opens
    /// it.
    Result(usize),
    /// A cell of the open conversation's message `message`, at `cell` (a
    /// laid-out row and a column of the pane): where a drag starts. On the
    /// message's delivery mark, `mark`; on a link of its body, the link's
    /// index among the laid-out links.
    Block {
        message: usize,
        cell: (usize, usize),
        mark: bool,
        link: Option<usize>,
    },
    /// The mailbox state in the header. Transmits: a fetch.
    Mailbox,
    /// The peer's position in the header: the peer on the map.
    Position,
    /// Our position share's chip row (1273): a click stops the share.
    ShareChip,
    /// The compose box's text, at this character of it.
    Compose { at: usize },
    /// The word `send` on the compose box's border. Transmits: a send.
    Send,
    /// A button of the recording bar in the compose box, or its silence
    /// line's click (1276), by the verb it raises.
    Rec(super::recording::Verb),
    /// A footer button.
    Footer(FooterAct),
    /// Inside the open palette or detail view.
    InsideOverlay,
    /// Outside it, or anywhere while the help is up: a click closes it. The
    /// help is closed by any click, as in lnomad, because it fills an 80 x
    /// 24 screen and leaves no outside to click.
    OutsideOverlay,
    /// A pin on the open map, its marker or its name: opens the peer's
    /// conversation.
    Pin([u8; 16]),
}

/// The screen's parts for the model's screen and compose box.
pub fn screen<B: InputBuffer>(model: &UiModel<B>) -> Screen {
    Screen::of(model.screen.0, model.screen.1, model.compose_rows())
}

/// The rectangle inside a pane's border; the conversation pane has no
/// bottom border, the compose box's top border is under it.
fn inner(rect: Rect, bottom_border: bool) -> Rect {
    Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(1 + u16::from(bottom_border)),
    )
}

/// The columns a title drawn right-aligned in a border row (the top or the
/// bottom one) occupies: its text `" {words} "` ends one cell before the
/// corner. The first column of `words` and how many.
fn right_title(rect: Rect, words: &str) -> (u16, u16) {
    let len = words.chars().count() as u16;
    (rect.right().saturating_sub(2 + len), len)
}

/// Whether the cell is among `len` cells from `x` on row `y`.
fn on(col: u16, row: u16, x: u16, y: u16, len: u16) -> bool {
    row == y && col >= x && col < x.saturating_add(len)
}

/// The pane under the cell, overlays aside.
pub fn pane_at<B: InputBuffer>(model: &UiModel<B>, col: u16, row: u16) -> Option<Pane> {
    let screen = screen(model);
    let messages = match model.map.is_some() {
        true => Pane::Map,
        false => Pane::Messages,
    };
    [
        (screen.list, Pane::Conversations),
        (screen.messages, messages),
        (screen.compose, Pane::Compose),
    ]
    .into_iter()
    .find(|(rect, _)| rect_contains(*rect, col, row))
    .map(|(_, pane)| pane)
}

/// The footer slot under the cell that a click runs: a hover highlights
/// only these.
pub fn footer_slot_at<B: InputBuffer>(model: &UiModel<B>, col: u16, row: u16) -> Option<usize> {
    let shown = view(model);
    footer::layout(shown.footer, screen(model).footer)
        .into_iter()
        .find(|slot| slot.item.action != FooterAct::Row(None) && rect_contains(slot.rect, col, row))
        .map(|slot| slot.index)
}

/// What a click at the cell lands on.
pub fn target_at<B: InputBuffer>(model: &UiModel<B>, col: u16, row: u16) -> Target {
    let screen = screen(model);
    let shown = view(model);
    let slots = footer::layout(shown.footer.clone(), screen.footer);
    let button = slots
        .iter()
        .find(|slot| rect_contains(slot.rect, col, row))
        .map(|slot| slot.item.action);

    if model.help || model.palette.is_some() || model.detail_open() {
        if let (Some(_), Some(act)) = (&model.palette, button) {
            return Target::Footer(act);
        }
        if model.help {
            return Target::OutsideOverlay;
        }
        let area = screen.area();
        let overlay = match (&shown.detail, &shown.palette) {
            (Some(detail), _) => detail_layout(detail, area).0,
            (None, Some(palette)) => palette_rect(palette.rows.len(), area),
            (None, None) => area,
        };
        return match rect_contains(overlay, col, row) {
            true => Target::InsideOverlay,
            false => Target::OutsideOverlay,
        };
    }
    if rect_contains(screen.footer, col, row) {
        return match button {
            Some(FooterAct::Row(None)) | None => Target::Nothing,
            Some(act) => Target::Footer(act),
        };
    }
    if rect_contains(screen.list, col, row) {
        let (x, len) = right_title(screen.list, &shown.list_hint);
        if len > 0 && on(col, row, x, screen.list.bottom().saturating_sub(1), len) {
            return Target::SearchHint;
        }
        // The title is the reader's own unless a search, the blocked or
        // the archived hold the list: its dot, a space, its words.
        let own = model.search.is_none() && !model.show_blocked && !model.show_archived;
        let title = 2 + shown.conversations_title.chars().count() as u16;
        if own && on(col, row, screen.list.x + 1, screen.list.y, title) {
            return Target::Me;
        }
        return list_target(model, screen.list, col, row);
    }
    if rect_contains(screen.messages, col, row) {
        if let Some(map) = &shown.map {
            return match super::map::pin_at(&map.pins, map.area, col, row) {
                Some(peer) => Target::Pin(peer),
                None => Target::Pane(Pane::Map),
            };
        }
        return messages_target(model, &shown, screen.messages, col, row);
    }
    if rect_contains(screen.compose, col, row) {
        let (x, _) = right_title(screen.compose, &shown.compose_hint);
        if let Some(at) = word_at(&shown.compose_hint, "send") {
            if on(col, row, x + at as u16, screen.compose.y, 4) {
                return Target::Send;
            }
        }
        let box_ = inner(screen.compose, true);
        if !rect_contains(box_, col, row) {
            return Target::Pane(Pane::Compose);
        }
        // The recording bar in the field's place (1276): its buttons, and
        // no text to put a cursor in.
        if let Some(bar) = &shown.recording {
            let (x, y) = (usize::from(col - box_.x), usize::from(row - box_.y));
            return bar
                .buttons
                .iter()
                .find(|&&(r, c, len, _)| r == y && x >= c && x < c + len)
                .map_or(Target::Pane(Pane::Compose), |&(_, _, _, verb)| {
                    Target::Rec(verb)
                });
        }
        return Target::Compose {
            at: compose_index(
                &shown.compose,
                shown.compose_cursor,
                box_,
                col - box_.x,
                row - box_.y,
            ),
        };
    }
    Target::Nothing
}

/// The character index in `text` of the word `word` standing alone.
fn word_at(text: &str, word: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let want: Vec<char> = word.chars().collect();
    (0..chars.len().saturating_sub(want.len() - 1)).find(|&at| {
        chars[at..].starts_with(&want)
            && (at == 0 || chars[at - 1] == ' ')
            && chars.get(at + want.len()).is_none_or(|&c| c == ' ')
    })
}

/// The character a click at (`x`, `y`) inside the compose box lands before:
/// on the line drawn there, at the column drawn there, or at the line's end
/// past it.
fn compose_index(text: &str, cursor: usize, box_: Rect, x: u16, y: u16) -> usize {
    let (top, scroll, _, _) = super::compose_window(
        text,
        cursor,
        usize::from(box_.width),
        usize::from(box_.height),
    );
    let lines: Vec<&str> = text.split('\n').collect();
    let line = (top + usize::from(y)).min(lines.len() - 1);
    let column = (scroll + usize::from(x)).min(lines[line].chars().count());
    lines[..line]
        .iter()
        .map(|l| l.chars().count() + 1)
        .sum::<usize>()
        + column
}

/// A click in the conversation list, or in the search that took its place.
fn list_target<B: InputBuffer>(model: &UiModel<B>, list: Rect, col: u16, row: u16) -> Target {
    let mut area = inner(list, true);
    if !rect_contains(area, col, row) {
        return Target::Pane(Pane::Conversations);
    }
    if let Some(open) = &model.search {
        let r = usize::from(row - area.y);
        if r < search::FIELD_ROWS {
            return Target::SearchField;
        }
        if let Some(query) = model.query() {
            let below = usize::from(area.height).saturating_sub(search::FIELD_ROWS);
            let laid = search::rows(model, query, usize::from(area.width));
            let at = search::first_row(&laid, open.selected, below) + r - search::FIELD_ROWS;
            return laid
                .starts
                .iter()
                .position(|&start| at >= start && at < start + 2)
                .map_or(Target::Pane(Pane::Conversations), Target::Result);
        }
        // An empty field shows the plain list under it.
        area = Rect::new(
            area.x,
            area.y + search::FIELD_ROWS as u16,
            area.width,
            area.height.saturating_sub(search::FIELD_ROWS as u16),
        );
        if !rect_contains(area, col, row) {
            return Target::Pane(Pane::Conversations);
        }
    }
    if model.conversations.is_empty() {
        return Target::Pane(Pane::Conversations);
    }
    let r = usize::from(row - area.y);
    let order = model.order();
    let first = layout::list_first(model, usize::from(area.height));
    let Some(&index) = order.get(first + r / ENTRY_ROWS) else {
        return Target::Pane(Pane::Conversations);
    };
    match r % ENTRY_ROWS {
        0 => Target::Conversation(index),
        1 => {
            let badge = layout::badge_cells(&model.conversations[index], usize::from(area.width));
            match badge {
                Some((x, len)) if on(col, row, area.x + x as u16, row, len as u16) => {
                    Target::Badge(index)
                }
                _ => Target::Conversation(index),
            }
        }
        _ => Target::Pane(Pane::Conversations),
    }
}

/// The conversation pane's rows: how many header rows, and where the laid
/// out rows begin on screen.
struct Rows {
    area: Rect,
    header: usize,
    /// The first laid-out row on screen.
    top: usize,
    /// The blank rows above it, when the conversation is shorter than the
    /// pane.
    pad: usize,
    /// How many laid-out rows are on screen.
    shown: usize,
}

fn rows<B: InputBuffer>(model: &UiModel<B>, laid: &Laid) -> Rows {
    let screen = screen(model);
    let area = inner(screen.messages, false);
    let header = model.header_rows();
    let height = model.message_rows();
    let top = model.top_of(laid.rows.len());
    let shown = laid.rows.len().saturating_sub(top).min(height);
    Rows {
        area,
        header,
        top,
        pad: height - shown,
        shown,
    }
}

/// A click in the conversation pane.
fn messages_target<B: InputBuffer>(
    model: &UiModel<B>,
    shown: &super::View,
    pane: Rect,
    col: u16,
    row: u16,
) -> Target {
    let wide = model.sizes().messages.0 >= WIDE_HEADER;
    if !wide {
        let (x, len) = right_title(pane, &shown.messages_title_right);
        if on(col, row, x, pane.y, shown.position_len as u16) {
            return Target::Position;
        }
        if len > 0 && on(col, row, x, pane.y, len) {
            return Target::Mailbox;
        }
    }
    let laid = model.laid();
    let at = rows(model, &laid);
    if !rect_contains(at.area, col, row) {
        return Target::Pane(Pane::Messages);
    }
    let r = usize::from(row - at.area.y);
    let c = usize::from(col - at.area.x);
    if model.chip_row() == Some(r) {
        return Target::ShareChip;
    }
    if r < at.header {
        // The mailbox row: the peer's position from its second cell, the
        // mailbox's words flush right after it.
        let position = 1..1 + shown.position_len;
        if r == 0 && position.contains(&c) {
            return Target::Position;
        }
        let words = shown.messages.first().map(|line| {
            let first = line
                .cells
                .iter()
                .skip(position.end)
                .position(|cell| cell.ch != ' ')
                .map(|at| position.end + at);
            (first, line.cells.len())
        });
        return match words {
            Some((Some(first), end)) if r == 0 && c >= first && c < end => Target::Mailbox,
            _ => Target::Pane(Pane::Messages),
        };
    }
    let r = r - at.header;
    if r < at.pad || r - at.pad >= at.shown {
        return Target::Pane(Pane::Messages);
    }
    let laid_row = at.top + r - at.pad;
    let Some(message) = laid
        .spans
        .iter()
        .position(|span| laid_row >= span.start && laid_row < span.end)
    else {
        return Target::Pane(Pane::Messages);
    };
    let mark = laid.spans[message]
        .mark
        .is_some_and(|(mr, mc, len)| mr == laid_row && c >= mc && c < mc + len);
    let link = laid.rows[laid_row]
        .cells
        .get(c)
        .and_then(|cell| cell.link)
        .map(|id| id - 1);
    Target::Block {
        message,
        cell: (laid_row, c),
        mark,
        link,
    }
}

/// The terminal's viewport (1231): the open chat's peer and the keys of its
/// messages whose last line is on screen (1529: the chat is read by the
/// line, so a message is read once its end was in view, not when its
/// bubble was selected or half of it showed). Nothing while the map, the
/// help or a detail view covers the conversation. A frontend of its own
/// layout says its own.
pub fn viewport<B: InputBuffer>(model: &UiModel<B>) -> Option<([u8; 16], Vec<u64>)> {
    let conversation = model.conversations.get(model.selected)?;
    let peer = conversation.peer;
    if model.map.is_some() || model.help || model.detail_open() {
        return Some((peer, Vec::new()));
    }
    let laid = model.laid();
    let at = rows(model, &laid);
    let keys = laid
        .spans
        .iter()
        .zip(&conversation.messages)
        .filter(|(span, _)| {
            let lines = super::pager::Lines {
                start: span.start,
                end: span.end,
            };
            super::pager::last_line_shown(lines, at.top, at.shown)
        })
        .map(|(_, message)| message.activity)
        .collect();
    Some((peer, keys))
}

/// The laid-out row and column under the cell for a drag, held to the
/// conversation's rows on screen: a drag past the pane's edge selects to
/// the edge.
pub fn pane_cell<B: InputBuffer>(model: &UiModel<B>, col: u16, row: u16) -> Option<(usize, usize)> {
    let laid = model.laid();
    let at = rows(model, &laid);
    if at.shown == 0 {
        return None;
    }
    let first = at.area.y as usize + at.header + at.pad;
    let r = (row as usize).clamp(first, first + at.shown - 1) - first;
    let c = (col as usize)
        .saturating_sub(at.area.x as usize)
        .min(usize::from(at.area.width).saturating_sub(1));
    Some((at.top + r, c))
}

/// A selection's two ends in reading order.
pub fn ordered(selection: &Selection) -> ((usize, usize), (usize, usize)) {
    match selection.anchor <= selection.head {
        true => (selection.anchor, selection.head),
        false => (selection.head, selection.anchor),
    }
}

/// The columns of `text`'s row `row` between `from` and `to` (both ends
/// in), held to the row's text: the margin, the bar, the time and the mark
/// are never selected.
pub fn selected_columns(
    text: &RowText,
    row: usize,
    from: (usize, usize),
    to: (usize, usize),
) -> core::ops::Range<usize> {
    if row < from.0 || row > to.0 {
        return 0..0;
    }
    let lo = match row == from.0 {
        true => from.1.max(text.start),
        false => text.start,
    };
    let hi = match row == to.0 {
        true => (to.1 + 1).min(text.start + text.len),
        false => text.start + text.len,
    };
    lo..hi.max(lo)
}

/// The text a selection covers, in reading order: each row's selected
/// text, a wrapped row joined to its line by the space the wrap took, a new
/// line between a body's lines and between two messages.
pub fn selected_text(laid: &Laid, selection: &Selection) -> String {
    let (from, to) = ordered(selection);
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for row in from.0..=to.0.min(laid.rows.len().saturating_sub(1)) {
        let Some(Some(text)) = laid.texts.get(row) else {
            continue;
        };
        let cols = selected_columns(text, row, from, to);
        // A first or last row the selection only touches outside its text
        // adds nothing; a middle row adds its line, empty or not.
        if cols.is_empty() && (row == from.0 || row == to.0) && text.len > 0 {
            continue;
        }
        if let Some(previous) = last {
            out.push(match previous == text.message && text.wrapped {
                true => ' ',
                false => '\n',
            });
        }
        out.extend(
            laid.rows[row]
                .cells
                .get(cols)
                .into_iter()
                .flatten()
                .map(|cell| cell.ch),
        );
        last = Some(text.message);
    }
    out
}
