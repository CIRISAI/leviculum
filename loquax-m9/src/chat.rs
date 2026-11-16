//! The open chat as styled lines on the M9's cell grid, under the top bar
//! [`crate::screen::lines`] draws: the bubbles, the compose line at the
//! bottom.
//!
//! The scene's chat (`Chat`, `loquax::ui::scene`) words and places it
//! already: its groups, a group's day line and writer, a bubble's time and
//! mark ([`loquax::ui::chat::Placement`]). This sets it in cells as the
//! terminal sets a conversation (`loquax::ui::layout::conversation`): a
//! bubble is its lines wrapped to three quarters of the grid
//! ([`loquax::ui::layout::block_cap`]), tinted with a margin cell either
//! side, ours flush right and theirs flush left; the time and mark close
//! its last line; a blank row between two groups. A group's own lines
//! (who joined, who left) are centred notes, and a line that arrived after
//! newer ones says so under it. The mark stands on its ASCII rung and every
//! line is [`crate::screen::narrow`]ed, as on the list.
//!
//! Only what fits is laid out: the bubbles from the bottom up, from the
//! newest or, when the cursor stands above the last screen, from the
//! cursor's bubble. The cursor's bubble is drawn behind the bar. The
//! window is a function of the scene and the cursor alone, so a redraw
//! draws what a fresh draw draws.
//!
//! Once the reader scrolled by the line (1529), the ring's up and down a
//! line and its left and right a screen less one ([`page`]), the view is
//! the model's anchor too: the scene's window is laid whole and drawn from
//! the line the anchor names, as long as the cursor's bubble shows there.
//! The rule that moves the line and the cursor is the terminal's
//! ([`loquax::ui::pager`]); this hands it the M9's lines.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::delivery::glyph;
use leviculum_ui::input::InputBuffer;
use leviculum_ui::keymap::ScrollCmd;
use leviculum_ui::style::{RLine, RStyle};
use leviculum_ui::text::fit_width;
use leviculum_ui::tokens::Paint;
use loquax::ui::layout::{block_cap, date_line, note_line};
use loquax::ui::pager::{last_line_shown, Lines, Window};
use loquax::ui::scene::{BubbleNode, Chat, Compose, Group};
use loquax::ui::strings::{StrId, Text};
use loquax::ui::{update, UiEffect, UiEvent, UiModel};

use crate::screen::{cut, fill, muted, narrow, painted, push, state_of, Look, BAR, NARROW};

/// The chat's `rows` lines under the top bar, `cols` cells each: our
/// share's chip, the bubbles, the compose line (or the line that the group
/// ended, or a group's invitation above the compose line).
/// With them the key of the topmost bubble on screen, whole or cut
/// (1533, the `[PAGE]` line's `top`); none while no bubble is. And the
/// keys of the bubbles whose last line is on screen (1541), in order: what
/// the reader saw, by the read rule of 1529 (`loquax::ui::pager::
/// last_line_shown`), which the board says as the model's viewport
/// ([`crate::read`]).
pub(crate) fn lines_at(
    chat: &Chat,
    look: &Look,
    cols: usize,
    rows: usize,
) -> (Vec<RLine>, Option<u64>, Vec<u64>) {
    let (head, foot) = parts(chat, look, cols);
    let area = rows.saturating_sub(head.len() + foot.len());
    let mut out = head;
    let (rows_drawn, top, seen) = match look.anchor {
        Some(_) => windowed(chat, look, cols, area),
        None => bubbles(chat, look, cols, area),
    };
    out.extend(rows_drawn);
    out.extend(foot);
    out.truncate(rows);
    (out, top, seen)
}

/// The rows above the bubbles (our share's chip) and under them (the
/// compose line, the line that the group ended, an invitation).
fn parts(chat: &Chat, look: &Look, cols: usize) -> (Vec<RLine>, Vec<RLine>) {
    let mut head = Vec::new();
    if let Some(share) = &chat.share {
        head.push(fill(
            styled(&cut(&share.words.resolve(look.lang), cols), muted(look)),
            cols,
            RStyle::default(),
        ));
    }
    let mut foot = Vec::new();
    if let Some(invite) = &chat.invite {
        let words = cut(&invite.title.resolve(look.lang), cols.saturating_sub(2));
        foot.push(fill(
            styled(&format!(" {words}"), painted(look.tokens.unread)),
            cols,
            RStyle::default(),
        ));
        // The card's Join (slice six, 1366): the trust sentence read once
        // before it, and the key that joins.
        if let Some(join) = &invite.join {
            let trust = narrow(&invite.trust.resolve(look.lang));
            let wrapped = loquax::ui::wrap(&[styled(&trust, muted(look))], cols - 2, 0);
            for line in wrapped {
                let mut row = styled(" ", RStyle::default());
                row.cells.extend(line.cells);
                foot.push(fill(row, cols, RStyle::default()));
            }
            let mut keys = styled(" ", RStyle::default());
            push(
                &mut keys,
                "[Enter]",
                RStyle {
                    bold: true,
                    ..RStyle::default()
                },
            );
            let label = Text::key(join.label_key).resolve(look.lang);
            push(
                &mut keys,
                &format!(" {}", narrow(&label)),
                RStyle::default(),
            );
            foot.push(fill(keys, cols, RStyle::default()));
        }
    }
    match &chat.ended {
        Some(ended) => foot.push(fill(
            note_line(&narrow(&ended.resolve(look.lang)), cols, look.tokens),
            cols,
            RStyle::default(),
        )),
        None => foot.push(compose_line(&chat.compose, look, cols)),
    }
    (head, foot)
}

/// A scroll key in the open chat (1529): the chat's window as the M9 lays
/// it, its lines and where its view stands, handed to the model, whose
/// rule ([`loquax::ui::pager`]) is the terminal's. What the model hands
/// out, as `update` does.
pub fn page<B: InputBuffer + Default>(model: &mut UiModel<B>, cmd: ScrollCmd) -> Vec<UiEffect> {
    let scene = crate::screen::scene(model);
    let (None, Some(chat)) = (&scene.list, &scene.chat) else {
        return Vec::new();
    };
    let look = Look::of(model);
    // The grid of the size the board views it in (1532).
    let (cols, rows) = crate::screen::size_of(model).grid();
    let below = rows.saturating_sub(1 + usize::from(scene.tabs.is_some()));
    let (head, foot) = parts(chat, &look, cols);
    let area = below.saturating_sub(head.len() + foot.len());
    let (window, _) = window(chat, &look, cols, area);
    update(model, UiEvent::Lines { cmd, window })
}

/// Every bubble of the scene's window laid (1529), each one's rows, and
/// the window: the bubbles' keys and lines (the separators above each
/// left out), all its rows, the `area` on screen, and where the view
/// stands. The view is the look's anchor where it names a bubble of the
/// window and the cursor's bubble shows there; otherwise where
/// [`bubbles`] stands: the newest at the bottom, or the screen ending at
/// the cursor's bubble when it stands above the last screen.
fn window(chat: &Chat, look: &Look, cols: usize, area: usize) -> (Window, Vec<Vec<RLine>>) {
    let at: Vec<(usize, usize)> = chat
        .groups
        .iter()
        .enumerate()
        .flat_map(|(g, group)| (0..group.bubbles.len()).map(move |b| (g, b)))
        .collect();
    let cursor = look.cursor.and_then(|key| {
        at.iter()
            .position(|&(g, b)| chat.groups[g].bubbles[b].bubble.key == key)
    });
    let mut window = Window {
        rows: area,
        ..Window::default()
    };
    let mut laid = Vec::with_capacity(at.len());
    let mut starts = Vec::with_capacity(at.len());
    for (k, &(g, b)) in at.iter().enumerate() {
        let rows = rows_of(chat, g, b, look, cols, cursor == Some(k));
        let start = window.total;
        starts.push(start);
        window.keys.push(chat.groups[g].bubbles[b].bubble.key);
        window.lines.push(Lines {
            start: (start + head_of(chat, g, b)).min(start + rows.len()),
            end: start + rows.len(),
        });
        window.total += rows.len();
        laid.push(rows);
    }
    let fallback = match cursor.or(at.len().checked_sub(1)) {
        None => 0,
        Some(a) => {
            let end = match window.total - starts[a] > area {
                true => starts[a] + laid[a].len(),
                false => window.total,
            };
            end.saturating_sub(area)
        }
    };
    let anchored = look
        .anchor
        .and_then(|anchor| window.row_of(anchor))
        .filter(|&top| cursor.is_none_or(|c| window.lines[c].shown(top, area)));
    window.top = anchored.unwrap_or(fallback);
    (window, laid)
}

/// The `area` rows of the scene's window from where the reader scrolled
/// it by the line (1529, [`window`]); blank rows above when the chat is
/// shorter; the key of the topmost bubble on screen; and the keys of the
/// bubbles whose last line is on screen (1541).
fn windowed(
    chat: &Chat,
    look: &Look,
    cols: usize,
    area: usize,
) -> (Vec<RLine>, Option<u64>, Vec<u64>) {
    let (window, laid) = window(chat, look, cols, area);
    let top = window.top;
    let seen = window
        .keys
        .iter()
        .zip(&window.lines)
        .filter(|(_, lines)| last_line_shown(**lines, top, area))
        .map(|(key, _)| *key)
        .collect();
    let shown: Vec<RLine> = laid.into_iter().flatten().skip(top).take(area).collect();
    let mut rows = Vec::with_capacity(area);
    rows.extend((shown.len()..area).map(|_| fill(RLine::default(), cols, RStyle::default())));
    rows.extend(
        shown
            .into_iter()
            .map(|row| fill(row, cols, RStyle::default())),
    );
    let key = window.anchor(top).map(|anchor| anchor.key);
    (rows, key, seen)
}

/// How many rows stand above bubble `b` of group `g` before its own: the
/// blank row after the group above, the day line, the writer's name
/// ([`rows_of`]'s head).
fn head_of(chat: &Chat, g: usize, b: usize) -> usize {
    let group = &chat.groups[g];
    if b != 0 {
        return 0;
    }
    usize::from(g > 0)
        + usize::from(group.day.is_some())
        + usize::from(group.named && !group.bubbles[b].bubble.note)
}

/// The `area` rows of bubbles, the newest at the bottom, or the cursor's
/// bubble there when it stands above the last screen; when the bubbles
/// from the first to the cursor's do not fill the screen, the first at the
/// top and the newer ones under it; blank rows above only when the chat
/// is shorter; the key of the topmost bubble drawn; and the keys of the
/// bubbles whose last line is on screen (1541).
fn bubbles(
    chat: &Chat,
    look: &Look,
    cols: usize,
    area: usize,
) -> (Vec<RLine>, Option<u64>, Vec<u64>) {
    let at: Vec<(usize, usize)> = chat
        .groups
        .iter()
        .enumerate()
        .flat_map(|(g, group)| (0..group.bubbles.len()).map(move |b| (g, b)))
        .collect();
    let cursor = look.cursor.and_then(|key| {
        at.iter()
            .position(|&(g, b)| chat.groups[g].bubbles[b].bubble.key == key)
    });
    let anchor = cursor.unwrap_or(at.len().saturating_sub(1));
    let item = |k: usize| {
        let (g, b) = at[k];
        rows_of(chat, g, b, look, cols, cursor == Some(k))
    };
    // The last screen holds the anchor whole: it ends at the newest.
    // Otherwise the screen ends at the anchor.
    let mut end = at.len();
    let mut used = 0;
    for k in (0..at.len()).rev() {
        used += item(k).len();
        if used > area {
            end = anchor + 1;
            break;
        }
        if k == anchor {
            break;
        }
    }
    let mut drawn: Vec<Vec<RLine>> = Vec::new();
    // The bubbles drawn, bottom-up first: by index, top to bottom below.
    let mut order: Vec<usize> = Vec::new();
    let mut used = 0;
    let mut top = None;
    for k in (0..end).rev() {
        let rows = item(k);
        used += rows.len();
        drawn.push(rows);
        order.push(k);
        let (g, b) = at[k];
        top = Some(chat.groups[g].bubbles[b].bubble.key);
        if used >= area {
            break;
        }
    }
    order.reverse();
    let mut lens: Vec<usize> = drawn.iter().rev().map(Vec::len).collect();
    let mut out: Vec<RLine> = drawn.into_iter().rev().flatten().collect();
    let cut = out.len().saturating_sub(area);
    out.drain(..cut);
    // The screen that ends at the anchor reached the chat's first bubble
    // with rows to spare (1544): the view clamps to the first bubble, and
    // the newer ones fill the rows under the anchor. Blank rows stay only
    // when the whole chat is shorter than the pane.
    for k in end..at.len() {
        if out.len() >= area {
            break;
        }
        let rows = item(k);
        order.push(k);
        lens.push(rows.len());
        out.extend(rows);
    }
    // What the reader sees (1541): the bubbles whose last line stands in
    // the rows kept, `cut` down from the first row drawn, `area` of them.
    let mut seen = Vec::new();
    let mut start = 0;
    for (&k, &len) in order.iter().zip(&lens) {
        let (g, b) = at[k];
        let lines = Lines {
            start: (start + head_of(chat, g, b)).min(start + len),
            end: start + len,
        };
        if last_line_shown(lines, cut, area) {
            seen.push(chat.groups[g].bubbles[b].bubble.key);
        }
        start += len;
    }
    out.truncate(area);
    let blank = area - out.len();
    let mut rows = Vec::with_capacity(area);
    rows.extend((0..blank).map(|_| fill(RLine::default(), cols, RStyle::default())));
    rows.extend(
        out.into_iter()
            .map(|row| fill(row, cols, RStyle::default())),
    );
    (rows, top, seen)
}

/// Bubble `b` of group `g` and what stands above it: before a group's
/// first, the blank row after the group above, the day line and the
/// writer's name; under a late line, that it came late.
fn rows_of(
    chat: &Chat,
    g: usize,
    b: usize,
    look: &Look,
    cols: usize,
    selected: bool,
) -> Vec<RLine> {
    let group = &chat.groups[g];
    let node = &group.bubbles[b];
    let mut out = Vec::new();
    if b == 0 {
        if g > 0 {
            out.push(RLine::default());
        }
        if let Some(day) = &group.day {
            out.push(date_line(
                &narrow(&day.words.resolve(look.lang)),
                cols,
                look.tokens,
            ));
        }
        if group.named && !node.bubble.note {
            out.push(writer(group, look, cols));
        }
    }
    if node.bubble.note {
        out.push(note_line(&narrow(&node.bubble.body), cols, look.tokens));
        return out;
    }
    let (x, block) = block(node, look, cols, selected);
    out.extend(block);
    if node.bubble.late {
        let words = cut(
            &Text::key(StrId::GroupArrivedLate).resolve(look.lang),
            cols.saturating_sub(2),
        );
        let style = RStyle {
            italic: true,
            ..muted(look)
        };
        let lead = match node.bubble.ours {
            true => cols.saturating_sub(fit_width(&words) + 1),
            false => x,
        };
        let mut row = styled(&" ".repeat(lead), RStyle::default());
        push(&mut row, &words, style);
        out.push(row);
    }
    out
}

/// The writer's name above a group's first bubble in a group's chat.
fn writer(group: &Group, look: &Look, cols: usize) -> RLine {
    let name = cut(&group.sender.resolve(look.lang), cols.saturating_sub(2));
    let mut row = styled(" ", RStyle::default());
    push(
        &mut row,
        &name,
        RStyle {
            fg: look.tokens.their_time.fg,
            bold: true,
            ..RStyle::default()
        },
    );
    row
}

/// One bubble's rows and the column its text starts in: the quote, the
/// media in words, the text, the report's words, each wrapped to the cap;
/// the time and mark closing the last row when it shows them.
fn block(node: &BubbleNode, look: &Look, cols: usize, selected: bool) -> (usize, Vec<RLine>) {
    let bubble = &node.bubble;
    let cap = block_cap(cols);
    let lang = look.lang;
    let mut texts: Vec<(String, RStyle)> = Vec::new();
    if let Some(quote) = &bubble.quote {
        let first = quote.text.lines().next().unwrap_or("");
        texts.push((
            format!("> {first}"),
            RStyle {
                italic: true,
                ..RStyle::default()
            },
        ));
    }
    // A picture sent as a file (1662) is its file's line here: the M9
    // draws no thumbnail.
    if let Some(photo) = node.photo.as_ref().filter(|p| p.file.is_none()) {
        texts.push((
            format!("[#] {}", photo.words.resolve(lang)),
            RStyle::default(),
        ));
    }
    for file in &node.files {
        texts.push((
            format!("[f] {} - {}", file.name, file.size),
            RStyle::default(),
        ));
    }
    if let Some(voice) = &node.voice {
        texts.push((
            format!("[v] {}", voice.words.resolve(lang)),
            RStyle::default(),
        ));
    }
    if let Some(invite) = &node.invite {
        texts.push((invite.title.resolve(lang), RStyle::default()));
        if let Some(state) = &invite.state {
            texts.push((state.resolve(lang), RStyle::default()));
        }
        // Its Join, under the cursor's Enter (slice six, 1366).
        if let Some(join) = &invite.join {
            texts.push((
                format!("[Enter] {}", Text::key(join.label_key).resolve(lang)),
                RStyle {
                    bold: true,
                    ..RStyle::default()
                },
            ));
        }
    }
    if let Some(text) = &node.text {
        texts.extend(
            text.split('\n')
                .map(|t| (String::from(t), RStyle::default())),
        );
    }
    if let Some(live) = &node.live {
        texts.push((live.resolve(lang), RStyle::default()));
    }
    let mut rows: Vec<RLine> = Vec::new();
    for (text, style) in &texts {
        let line = styled(&narrow(text), *style);
        rows.extend(loquax::ui::wrap(&[line], cap, 0));
    }

    // The time and the mark, as the terminal closes a block.
    let time_style = match (selected, bubble.ours) {
        (true, _) => RStyle::default(),
        (false, true) => painted(look.tokens.own_time),
        (false, false) => painted(look.tokens.their_time),
    };
    let mut suffix = RLine::default();
    if node.place.meta {
        if let Some(at) = bubble.at {
            push(&mut suffix, "  ", RStyle::default());
            push(&mut suffix, &at.hh_mm(), time_style);
        }
        if let Some(mark) = &node.mark {
            let rung = glyph(state_of(mark.mark));
            push(&mut suffix, " ", RStyle::default());
            push(
                &mut suffix,
                &crate::screen::mark_glyph(rung),
                RStyle {
                    fg: look.tokens.tones.of(rung.tone),
                    bold: rung.bold,
                    ..RStyle::default()
                },
            );
        }
    }
    if !suffix.cells.is_empty() {
        match rows.last_mut() {
            Some(last) if last.cells.len() + suffix.cells.len() <= cols.saturating_sub(2) => {
                last.cells.extend(suffix.cells);
            }
            _ => {
                let cells = suffix.cells.into_iter().skip_while(|c| c.ch == ' ');
                rows.push(RLine {
                    cells: cells.collect(),
                });
            }
        }
    }

    let inner = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
    let x = match bubble.ours {
        true => cols.saturating_sub(inner + 1).max(1),
        false => 1,
    };
    let tokens = look.tokens;
    let paint = match (bubble.ours, selected) {
        (true, false) => tokens.own,
        (true, true) => tokens.own_selected,
        (false, false) => tokens.their,
        (false, true) => tokens.their_selected,
    };
    // A tint covers the margins; reverse video only the text.
    let (margin, text_paint) = match paint.reverse {
        true => (Paint::NONE, paint),
        false => (paint, paint),
    };
    let rows = rows
        .into_iter()
        .map(|row| {
            let mut line = styled(&" ".repeat(x - 1), RStyle::default());
            let edge = match selected {
                true => BAR,
                false => ' ',
            };
            push(&mut line, edge.encode_utf8(&mut [0; 4]), painted(margin));
            let pad = inner - row.cells.len();
            let mut cells = row.cells;
            push_cells(&mut cells, pad);
            for cell in &mut cells {
                cell.st.fg = cell.st.fg.or(text_paint.fg);
                cell.st.bg = text_paint.bg;
                cell.st.reverse = text_paint.reverse;
            }
            line.cells.extend(cells);
            push(&mut line, " ", painted(margin));
            line
        })
        .collect();
    (x, rows)
}

/// The compose line: the field across the grid, a cell either side, in
/// the chrome. Its hint while it is empty and not typed into; while it is
/// typed into, the text scrolled so the caret shows, the caret's cell
/// reversed.
fn compose_line(compose: &Compose, look: &Look, cols: usize) -> RLine {
    let chrome = painted(look.tokens.chrome);
    let width = cols.saturating_sub(2).max(1);
    let mut row = styled(" ", chrome);
    match (compose.text.is_empty(), look.caret) {
        (true, None) => {
            let hint = cut(&compose.hint.resolve(look.lang), width);
            push(
                &mut row,
                &hint,
                RStyle {
                    fg: look.tokens.chrome_muted.fg.or(chrome.fg),
                    ..chrome
                },
            );
        }
        (_, None) => push(
            &mut row,
            &cut(&compose.text.replace('\n', " "), width),
            chrome,
        ),
        (_, Some(caret)) => {
            let (shown, at) = caret_cells(&compose.text, caret);
            let skip = at.saturating_sub(width - 1);
            for (i, ch) in shown.chars().chain(core::iter::once(' ')).enumerate() {
                if i < skip {
                    continue;
                }
                if i - skip == width {
                    break;
                }
                let style = RStyle {
                    reverse: i == at,
                    ..chrome
                };
                push(&mut row, ch.encode_utf8(&mut [0; 4]), style);
            }
        }
    }
    fill(row, cols, chrome)
}

/// `text` one cell a character as the panel draws it, a line break a
/// space, and the cell the caret before character `caret` stands on.
pub fn caret_cells(text: &str, caret: usize) -> (String, usize) {
    let mut shown = String::new();
    let mut at = None;
    for (i, ch) in text.chars().enumerate() {
        if i == caret {
            at = Some(shown.chars().count());
        }
        let ch = if ch == '\n' { ' ' } else { ch };
        match fit_width(ch.encode_utf8(&mut [0; 4])) {
            0 => {}
            1 => shown.push(ch),
            _ => shown.push(NARROW),
        }
    }
    let at = at.unwrap_or_else(|| shown.chars().count());
    (shown, at)
}

fn styled(text: &str, st: RStyle) -> RLine {
    let mut line = RLine::default();
    push(&mut line, text, st);
    line
}

fn push_cells(cells: &mut Vec<leviculum_ui::style::StyledChar>, pad: usize) {
    let mut line = RLine::default();
    push(&mut line, &" ".repeat(pad), RStyle::default());
    cells.extend(line.cells);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caret_counts_cells_not_characters() {
        // A joiner takes no cell, an emoji one, a line break is a space.
        assert_eq!(caret_cells("a\u{200d}b", 2), (String::from("ab"), 1));
        assert_eq!(caret_cells("😀x", 1), (String::from("?x"), 1));
        assert_eq!(caret_cells("a\nb", 3), (String::from("a b"), 3));
        assert_eq!(caret_cells("", 0), (String::new(), 0));
    }
}
