//! The chat layout in cells (`docs/src/concepts/loquax-keys-and-layout.md`,
//! section 5): a conversation as blocks with their date lines and the `new`
//! rule, and the list as two rows a conversation.
//!
//! Pure functions of the model's data, the pane's width and the time: the
//! renderer draws the rows as they come, and the model's scrolling reads the
//! same [`Laid`] to know where each message's block starts and ends. The
//! mouse reads it too ([`super::hit`]): which cells of a row are a body's
//! text, where a delivery mark is, which cells are a link.
//!
//! In the colour tokens of section 5, "Colour" ([`leviculum_ui::tokens`](mod@leviculum_ui::tokens)),
//! which [`Draw`] carries for the model's tier and theme: a block is a
//! rectangle of its tint, its text plus a margin cell either side; its time
//! takes the block's time colour, its mark the tone of its state; the date
//! takes a pill under its label, the `new` rule the unread colour, and the
//! top line and the dashes the muted one. A token that is reverse video (a
//! selected block in 16 colours and in monochrome) reverses the text alone,
//! as the monochrome screen always has.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use leviculum_ui::delivery::glyph;
use leviculum_ui::input::InputBuffer;
use leviculum_ui::link::find_links;
use leviculum_ui::text::{fit_width, truncate_to_cols};
use leviculum_ui::tokens::{Paint, Tokens};
use leviculum_ui::{RLine, RStyle};

use super::scene::PositionBadge;
use super::strings::{Arg, Lang, StrId, Text};
use super::{leg_label, plain, push, reactions, wrap, Conversation, Message, Signature, UiModel};
use crate::names::Names;
use crate::telemetry;
use crate::when::{date_label, list_time, WallTime};

/// The top of what this device holds: the store's first message with the
/// peer.
pub const EARLIEST: &str = "nothing earlier on this device";

/// The top of what the screen loaded, when the store holds more: reaching it
/// loads the next page.
pub fn earlier_line(count: usize) -> String {
    format!("{count} earlier on this device, scroll up for them")
}

/// The rule under the pinned conversations.
pub const PIN_RULE: char = '┄';

/// Below this pane width ours are drawn flush left, with `you` before the
/// time, because right alignment no longer reads as a side.
pub const NARROW: usize = 40;

/// The selection's bar in the margin.
pub const BAR: char = '▌';

/// What drawing a conversation needs besides the conversation.
pub struct Draw<'a> {
    pub now: Option<WallTime>,
    /// The spinner's frame and the legs' seconds.
    pub tick: u64,
    pub ascii: bool,
    /// The message drawn selected: the cursor's, while the pane has the focus.
    pub selected: Option<usize>,
    /// The colour tokens of the model's tier and theme.
    pub tokens: &'static Tokens,
    /// The open search's query: each block marks its first match in place,
    /// between `«` and `»` (section 5, the 120 x 40 mockup).
    pub mark: Option<&'a str>,
    /// A client of a home node: the home's `Sent` is the mailbox's tick
    /// ([`super::shown_delivery`]).
    pub home_client: bool,
    /// The reader's read receipts switch (1233): off, read is drawn
    /// delivered ([`super::shown_delivery`]).
    pub receipts: bool,
}

/// A style drawn in a token's colours.
pub fn painted(paint: Paint) -> RStyle {
    RStyle {
        fg: paint.fg,
        bg: paint.bg,
        reverse: paint.reverse,
        ..RStyle::default()
    }
}

/// Where one message's rows are, separators above it included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    /// The delivery mark's cells, a click target: the row, the first column
    /// and how many columns.
    pub mark: Option<(usize, usize, usize)>,
}

/// Which cells of a row are a message's text: what a drag copies, without
/// the margin, the bar, the time or the mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowText {
    /// The message's index in the conversation.
    pub message: usize,
    /// The text's first column in the pane.
    pub start: usize,
    /// Its columns, which may be none on an empty line of a body.
    pub len: usize,
    /// The row goes on the row above's line: the wrap swallowed a space
    /// between them.
    pub wrapped: bool,
}

/// A conversation laid out: every row from the top of what the device holds,
/// and each message's span among them.
pub struct Laid {
    pub rows: Vec<RLine>,
    /// Each row's text, beside `rows`: `None` on a separator, a blank and the
    /// time's own row.
    pub texts: Vec<Option<RowText>>,
    pub spans: Vec<Span>,
    /// The links in the bodies; a cell's [`leviculum_ui::StyledChar::link`]
    /// is its index here plus one.
    pub links: Vec<String>,
}

impl Laid {
    /// No conversation open: the pane says only where the device's memory
    /// begins.
    pub fn empty(width: usize, tokens: &Tokens) -> Self {
        Self {
            rows: vec![centred(EARLIEST, width, painted(tokens.muted))],
            texts: vec![None],
            spans: Vec::new(),
            links: Vec::new(),
        }
    }

    /// A row with no text on it.
    fn push(&mut self, row: RLine) {
        self.rows.push(row);
        self.texts.push(None);
    }
}

/// What was drawn last, for the blank row between blocks.
#[derive(PartialEq)]
enum Last {
    Separator,
    Block,
}

/// `conversation` as rows `width` cells wide: the top line, a centred date
/// before each day's first message, the `new` rule where the unread begins,
/// the blocks with a blank row between two of them.
pub fn conversation(conversation: &Conversation, width: usize, draw: &Draw<'_>) -> Laid {
    let width = width.max(1);
    let mut laid = Laid::empty(width, draw.tokens);
    if conversation.earlier > 0 {
        laid.rows[0] = centred(
            &earlier_line(conversation.earlier),
            width,
            painted(draw.tokens.muted),
        );
    }
    let mut last = Last::Separator;
    let mut day = None;
    // The first of the reactions and reports folded since the last block: a
    // note or the `new` rule that stood before one goes before the next
    // block instead.
    let mut skipped: Option<usize> = None;
    let reactions = reactions::gather(conversation);
    for (index, message) in conversation.messages.iter().enumerate() {
        let start = laid.rows.len();
        if folded(conversation, index) {
            skipped.get_or_insert(index);
            // Drawn as the report after it: no rows, so no click lands on
            // it and the cursor steps over it.
            laid.spans.push(Span {
                start,
                end: start,
                mark: None,
            });
            continue;
        }
        if let (Some(at), Some(now)) = (message.at, draw.now) {
            if day != Some(at.day()) {
                day = Some(at.day());
                if last == Last::Block {
                    laid.push(RLine::default());
                }
                laid.push(date_line(&date_label(&at, &now), width, draw.tokens));
                last = Last::Separator;
            }
        }
        let from = skipped.take().unwrap_or(index);
        let here = |at: usize| (from..=index).contains(&at);
        for note in conversation.notes.iter().filter(|n| here(n.before)) {
            if last == Last::Block {
                laid.push(RLine::default());
            }
            laid.push(note_line(&note.text, width, draw.tokens));
            last = Last::Separator;
        }
        if let Some(rule) = conversation.new_rule.filter(|rule| here(rule.at)) {
            if last == Last::Block {
                laid.push(RLine::default());
            }
            laid.push(new_rule(rule.count, width, draw.tokens));
            last = Last::Separator;
        }
        if last == Last::Block {
            laid.push(RLine::default());
        }
        let first = laid.rows.len();
        let drawn = block(
            conversation,
            index,
            width,
            draw,
            draw.selected == Some(index),
            &mut laid.links,
        );
        laid.rows.extend(drawn.rows);
        laid.texts.extend(drawn.texts);
        let chips = message.id.and_then(|id| reactions.get(&id));
        if let Some(row) =
            chips.and_then(|chips| reactions::row(chips, message.outbound, width, draw.tokens))
        {
            laid.push(row);
        }
        laid.spans.push(Span {
            start,
            end: laid.rows.len(),
            mark: drawn.mark.map(|(row, col, len)| (first + row, col, len)),
        });
        last = Last::Block;
    }
    let after = skipped.unwrap_or(conversation.messages.len());
    for note in conversation.notes.iter().filter(|n| n.before >= after) {
        if last == Last::Block {
            laid.push(RLine::default());
        }
        laid.push(note_line(&note.text, width, draw.tokens));
        last = Last::Separator;
    }
    laid
}

/// Whether message `index` has no block of its own: a reaction, drawn
/// under its target ([`reactions`]), or a telemetry report the next one
/// replaces on screen: one peer's consecutive reports fold into the latest
/// (`loquax-map.md`, "In the conversation"), by the rule every screen folds
/// by ([`Conversation::report_folded`]).
pub fn folded(conversation: &Conversation, index: usize) -> bool {
    conversation
        .messages
        .get(index)
        .is_some_and(Message::is_reaction)
        || conversation.report_folded(index)
}

/// `── was Alice, now announces Ally ──`, centred and muted: a peer's name
/// changed, said once where it happened (`loquax-mailbox.md`, the naming
/// decision, 2). Cut to the pane with `…` when it is wider.
fn note_line(text: &str, width: usize, tokens: &Tokens) -> RLine {
    let text = truncate_to_cols(&format!("── {text} ──"), width.saturating_sub(2));
    centred(&text, width, painted(tokens.muted))
}

/// `text` centred in `width` cells, in `st`.
fn centred(text: &str, width: usize, st: RStyle) -> RLine {
    let pad = width.saturating_sub(fit_width(text)) / 2;
    let mut line = plain(&" ".repeat(pad));
    push(&mut line, text, st);
    line
}

/// `── Saturday 24 October ──`, centred: the dashes muted, the label with a
/// space either side on the date pill (decided 2026-10-26, part 6, 3).
fn date_line(label: &str, width: usize, tokens: &Tokens) -> RLine {
    let text = format!("── {label} ──");
    let pad = width.saturating_sub(text.chars().count()) / 2;
    let muted = painted(tokens.muted);
    let mut line = plain(&" ".repeat(pad));
    push(&mut line, "──", muted);
    push(&mut line, &format!(" {label} "), painted(tokens.date_pill));
    push(&mut line, "──", muted);
    line
}

/// ` ───── 2 new ───── `, the pane's width less a margin cell either side.
/// The rule and its count in the unread colour.
pub fn new_rule(count: usize, width: usize, tokens: &Tokens) -> RLine {
    let label = format!(" {count} new ");
    let inner = width.saturating_sub(2);
    let dashes = inner.saturating_sub(label.chars().count());
    let left = dashes / 2;
    let mut line = plain(" ");
    push(
        &mut line,
        &format!("{}{label}{}", "─".repeat(left), "─".repeat(dashes - left)),
        painted(tokens.unread),
    );
    push(&mut line, " ", RStyle::default());
    line
}

/// The widest a block's text may be: three quarters of the pane less the
/// margin cell either side, or the whole pane on a narrow one.
pub fn block_cap(width: usize) -> usize {
    match width < NARROW {
        true => width.saturating_sub(2),
        false => (width * 3 / 4).saturating_sub(2),
    }
    .max(1)
}

/// What closes a block's last line: the leg a waiting send is on, `you` on a
/// narrow pane, the time, and for ours the delivery mark in its tone. The
/// words take the block's time colour, unless the block is selected, where
/// they take its text colour; a mark without a tone (16 colours) takes the
/// text colour too.
fn suffix(message: &Message, narrow: bool, draw: &Draw<'_>, selected: bool) -> RLine {
    let mut words: Vec<String> = Vec::new();
    if message.outbound && narrow {
        words.push("you".into());
    }
    if let Some(at) = message.at {
        words.push(at.hh_mm());
    }
    let mut text = words.join(" ");
    if let Some(label) = message
        .delivery
        .and_then(|state| leg_label(message, state, draw.tick))
    {
        text = match text.is_empty() {
            true => label,
            false => format!("{label} · {text}"),
        };
    }
    let doubt = match (message.outbound, message.signature) {
        (false, Signature::Unverified) => Some("unverified"),
        (false, Signature::Invalid) => Some("INVALID signature"),
        _ => None,
    };
    if let Some(doubt) = doubt {
        text = match text.is_empty() {
            true => doubt.into(),
            false => format!("{text} · {doubt}"),
        };
    }
    let time = match (selected, message.outbound) {
        (true, _) => RStyle::default(),
        (false, true) => painted(draw.tokens.own_time),
        (false, false) => painted(draw.tokens.their_time),
    };
    let mut line = RLine::default();
    if !text.is_empty() {
        push(&mut line, "  ", RStyle::default());
        push(&mut line, &text, time);
    }
    if let Some(state) = message.delivery {
        let row = glyph(super::shown_delivery(
            state,
            draw.home_client,
            draw.receipts,
        ));
        push(
            &mut line,
            if text.is_empty() { "  " } else { " " },
            RStyle::default(),
        );
        let mark = RStyle {
            fg: draw.tokens.tones.of(row.tone),
            bold: row.bold,
            ..RStyle::default()
        };
        push(&mut line, row.symbol(draw.tick as usize, draw.ascii), mark);
    }
    line
}

/// A block's rows, which cells of each are text, and where its mark is
/// among them.
struct Block {
    rows: Vec<RLine>,
    texts: Vec<Option<RowText>>,
    /// The row in the block, the first column and the columns.
    mark: Option<(usize, usize, usize)>,
}

/// One message's block: its lines wrapped at a space to the cap, the suffix
/// closing the last one, ours flush right and theirs flush left with a margin
/// cell either side, the selected one behind the bar. The block's token
/// tints the text and both margin cells; a reverse-video token (a selected
/// block in 16 colours and monochrome) reverses the text alone. The suffix may
/// reach past the cap into the pane's last quarter, as in both mockups, and
/// takes a row of its own only where the pane itself has no room for it. A
/// link in the body is underlined and its cells name it in `links`. With a
/// search open the body's first match is marked, the marks part of the text.
fn block(
    conversation: &Conversation,
    index: usize,
    width: usize,
    draw: &Draw<'_>,
    selected: bool,
    links: &mut Vec<String>,
) -> Block {
    let peer = conversation.peer;
    let message = &conversation.messages[index];
    let narrow = width < NARROW;
    let cap = block_cap(width);
    let mut rows = Vec::new();
    let mut wrapped = Vec::new();
    let body = match (message.is_report(), draw.mark) {
        // A report has no text: its line says what it carried.
        (true, _) => {
            let received = message.at.map(|at| at.unix_secs);
            let read = telemetry::read(peer, &message.fields, received);
            // The newest report says whether the position is alive
            // (1209); an older run, which stands for all of it, its count
            // (1184).
            let newest = conversation.newest_fix_report();
            let words = match newest == Some(index) {
                true => {
                    let now = draw.now.map(|now| now.unix_secs);
                    format!(" · {}", conversation.liveness(now).row_words(now))
                }
                false => {
                    let run = conversation.run(index);
                    let since = run
                        .first()
                        .and_then(|&first| conversation.messages[first].at);
                    telemetry::run_words(run.len(), since.as_ref())
                }
            };
            format!("{}{words}", telemetry::report_line(&read))
        }
        (false, Some(query)) => super::search::mark_first(&message.body, query),
        (false, None) => message.body.clone(),
    };
    // A text that came with a position says so under it, in a report's
    // words: `Enter` on it opens the map there.
    let body = match (message.is_report(), super::message_fix(peer, message)) {
        (false, Some(_)) => {
            let received = message.at.map(|at| at.unix_secs);
            let read = telemetry::read(peer, &message.fields, received);
            format!("{body}\n{}", telemetry::report_line(&read))
        }
        _ => body,
    };
    // A photo (1220) says so under its text, or alone: `Enter` on it
    // shows it whole in the system's viewer.
    let body = match super::photos::photo_of(&message.fields) {
        Some(photo) => {
            let (glyph, dot) = match draw.ascii {
                true => ("[#]", "-"),
                false => ("▣", "·"),
            };
            let line = format!(
                "{glyph} photo {dot} {}",
                super::photos::size_words(photo.bytes)
            );
            match body.trim().is_empty() {
                true => line,
                false => format!("{body}\n{line}"),
            }
        }
        None => body,
    };
    // A file (1221) says its name and size under its text, or alone:
    // `Enter` on it saves it under the home and opens it.
    let body = super::files::files_of(&message.fields)
        .into_iter()
        .fold(body, |body, file| {
            let (glyph, dot) = match draw.ascii {
                true => ("[f]", "-"),
                false => ("▤", "·"),
            };
            let line = format!(
                "{glyph} {} {dot} {}",
                super::files::safe_name(&file.name),
                super::photos::size_words(file.bytes)
            );
            match body.trim().is_empty() {
                true => line,
                false => format!("{body}\n{line}"),
            }
        });
    // A reaction drawn as a block, its signature broken: what it claims.
    let body = match (body.trim().is_empty(), message.reaction()) {
        (true, Some((_, content))) => format!("reacted {content}"),
        _ => body,
    };
    for text in body.split('\n') {
        let mut line = plain(text);
        for found in find_links(text) {
            links.push(found.target);
            let id = links.len();
            for cell in &mut line.cells[found.start..found.end] {
                cell.link = Some(id);
                cell.st.underline = true;
            }
        }
        for (n, row) in wrap(&[line], cap, 0).into_iter().enumerate() {
            rows.push(row);
            wrapped.push(n > 0);
        }
    }
    let text_lens: Vec<usize> = rows.iter().map(|row| row.cells.len()).collect();
    let suffix = suffix(message, narrow, draw, selected);
    if !suffix.cells.is_empty() {
        match rows.last_mut() {
            Some(last) if last.cells.len() + suffix.cells.len() <= width.saturating_sub(2) => {
                last.cells.extend(suffix.cells);
            }
            _ => {
                // On a row of its own, the leading spaces dropped.
                let cells: Vec<_> = suffix
                    .cells
                    .into_iter()
                    .skip_while(|c| c.ch == ' ')
                    .collect();
                rows.push(RLine { cells });
            }
        }
    }
    let inner = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
    let x = match message.outbound && !narrow {
        true => width.saturating_sub(inner + 1).max(1),
        false => 1,
    };
    // The mark closes the last row: its cells are that row's last.
    let mark = message.delivery.and_then(|state| {
        let len = glyph(super::shown_delivery(
            state,
            draw.home_client,
            draw.receipts,
        ))
        .symbol(draw.tick as usize, draw.ascii)
        .chars()
        .count();
        let row = rows.len().checked_sub(1)?;
        Some((row, x + rows[row].cells.len() - len, len))
    });
    let texts = (0..rows.len())
        .map(|row| {
            text_lens.get(row).map(|&len| RowText {
                message: index,
                start: x,
                len,
                wrapped: wrapped[row],
            })
        })
        .collect();
    let tokens = draw.tokens;
    let paint = match (message.outbound, selected) {
        (true, false) => tokens.own,
        (true, true) => tokens.own_selected,
        (false, false) => tokens.their,
        (false, true) => tokens.their_selected,
    };
    // A tint covers the margins; reverse video only the text, as it always
    // has.
    let (margin, text_paint) = match paint.reverse {
        true => (Paint::NONE, paint),
        false => (paint, paint),
    };
    let rows = rows
        .into_iter()
        .map(|row| {
            let mut line = RLine::default();
            push(&mut line, &" ".repeat(x - 1), RStyle::default());
            match selected {
                true => push(&mut line, &String::from(BAR), painted(margin)),
                false => push(&mut line, " ", painted(margin)),
            }
            let pad = inner - row.cells.len();
            let mut cells = row.cells;
            cells.extend(plain(&" ".repeat(pad)).cells);
            for cell in &mut cells {
                cell.st.fg = cell.st.fg.or(text_paint.fg);
                cell.st.bg = text_paint.bg;
                cell.st.reverse = text_paint.reverse;
            }
            line.cells.extend(cells);
            if !margin.is_none() {
                push(&mut line, " ", painted(margin));
            }
            line
        })
        .collect();
    Block { rows, texts, mark }
}

/// Rows a list entry takes: name and time, preview and badge, a blank.
pub const ENTRY_ROWS: usize = 3;

/// How many conversations a list `height` rows high shows whole; the last
/// one's blank row may fall off the bottom.
pub fn entries_shown(height: usize) -> usize {
    (height + 1) / ENTRY_ROWS
}

/// The list's rows on screen: two a conversation in the list's order, the
/// selected one's behind the bar and, under focus, reversed, scrolled just
/// far enough to show it.
pub fn list<B: InputBuffer>(model: &UiModel<B>, width: usize, height: usize) -> Vec<RLine> {
    let order = model.order();
    let first = list_first(model, height);
    let focused = model.focus == super::Pane::Conversations;
    let mut rows = Vec::new();
    // The rule goes in the blank row under the last pinned conversation,
    // when another follows it.
    let last_pinned = order
        .iter()
        .rposition(|&i| model.conversations[i].pinned)
        .filter(|&at| at + 1 < order.len())
        .map(|at| order[at]);
    for &index in &order[first..] {
        let conversation = &model.conversations[index];
        let here = index == model.selected;
        let [top, bottom] = entry(conversation, &model.names, width, model.now, here);
        for mut row in [top, bottom] {
            if here {
                if let Some(cell) = row.cells.first_mut() {
                    cell.ch = BAR;
                }
                if focused {
                    let selection = painted(model.tokens().selection);
                    let end = row.cells.len().saturating_sub(1);
                    for cell in row.cells.iter_mut().take(end).skip(1) {
                        cell.st.fg = selection.fg.or(cell.st.fg);
                        cell.st.bg = selection.bg.or(cell.st.bg);
                        cell.st.reverse = selection.reverse;
                    }
                }
            }
            rows.push(row);
        }
        match last_pinned == Some(index) {
            true => {
                let mut rule = RLine::default();
                push(&mut rule, " ", RStyle::default());
                let dots: String =
                    core::iter::repeat_n(PIN_RULE, width.saturating_sub(2)).collect();
                push(&mut rule, &dots, painted(model.tokens().muted));
                push(&mut rule, " ", RStyle::default());
                rows.push(rule);
            }
            false => rows.push(RLine::default()),
        }
    }
    rows.truncate(height);
    rows
}

/// The first conversation on screen, in the list's order, of a list
/// `height` rows high: scrolled just far enough to show the selected one.
pub fn list_first<B>(model: &UiModel<B>, height: usize) -> usize {
    let at = model
        .order()
        .iter()
        .position(|&i| i == model.selected)
        .unwrap_or(0);
    (at + 1).saturating_sub(entries_shown(height).max(1))
}

/// The badge's columns in a list entry's second row `width` cells wide: the
/// first and how many, `[3]` flush right before the last cell.
pub fn badge_cells(conversation: &Conversation, width: usize) -> Option<(usize, usize)> {
    match conversation.unread_rows() {
        0 => None,
        n => {
            let len = format!("[{n}]").chars().count();
            Some((width.saturating_sub(1 + len), len))
        }
    }
}

/// One conversation's two rows, `width` cells with the bar's margin first
/// and one blank last: the name and the last message's time, the last line's
/// preview (`you:` when ours) and the unread count in brackets, reversed.
/// A conversation not `open` with a message typed and not sent shows that
/// instead of the last line, `Draft: …` in italics (1219).
fn entry(
    conversation: &Conversation,
    names: &Names,
    width: usize,
    now: Option<WallTime>,
    open: bool,
) -> [RLine; 2] {
    let room = width.saturating_sub(2);
    let last = conversation.messages.last();
    let time = match (last.and_then(|m| m.at), now) {
        (Some(at), Some(now)) => list_time(&at, &now),
        _ => String::new(),
    };
    let mut top = RLine::default();
    push(&mut top, " ", RStyle::default());
    let title = names.label_in(
        &conversation.peer,
        room.saturating_sub(time.chars().count() + 1),
    );
    let gap = room.saturating_sub(fit_width(&title) + time.chars().count());
    push(
        &mut top,
        &format!("{title}{}{time} ", " ".repeat(gap)),
        RStyle::default(),
    );

    let badge = match conversation.unread_rows() {
        0 => String::new(),
        n => format!("[{n}]"),
    };
    let preview = match last {
        Some(message) if message.is_reaction() => reactions::preview(message).unwrap_or_default(),
        // Whether the position is alive, not how many came (1209).
        Some(message) if message.is_report() => format!(
            "{} position · {}",
            telemetry::REPORT_MARK,
            conversation
                .liveness(now.map(|now| now.unix_secs))
                .list_words(now.map(|now| now.unix_secs))
        ),
        Some(message) => {
            let first = message.body.split('\n').next().unwrap_or("");
            match message.outbound {
                true => format!("you: {first}"),
                false => first.into(),
            }
        }
        None => String::new(),
    };
    // The map pin (1224, `scene::PositionBadge`'s rule): left of the
    // count, `m` on the entry opens the map on the peer.
    let now_secs = now.map(|now| now.unix_secs);
    let pin = PositionBadge::of(
        &conversation.peer,
        "",
        &conversation.liveness(now_secs),
        now_secs,
    )
    .map(|_| format!("{} ", StrId::PositionMarker.text(Lang::En)))
    .unwrap_or_default();
    let marks = pin.chars().count() + badge.chars().count();
    let budget = match marks {
        0 => room,
        n => room.saturating_sub(n + 1),
    };
    let draft = conversation.draft.line().filter(|_| !open);
    let preview = match &draft {
        Some(line) => Text::Key(StrId::DraftRow, vec![Arg::Str(line.clone())]).resolve(Lang::En),
        None => preview,
    };
    let preview = leviculum_ui::text::truncate_to_cols(&preview, budget);
    let gap = room.saturating_sub(fit_width(&preview) + marks);
    let mut bottom = RLine::default();
    push(&mut bottom, " ", RStyle::default());
    push(
        &mut bottom,
        &preview,
        RStyle {
            italic: draft.is_some(),
            ..RStyle::default()
        },
    );
    push(
        &mut bottom,
        &format!("{}{pin}", " ".repeat(gap)),
        RStyle::default(),
    );
    push(
        &mut bottom,
        &badge,
        RStyle {
            reverse: true,
            ..RStyle::default()
        },
    );
    push(&mut bottom, " ", RStyle::default());
    [top, bottom]
}
