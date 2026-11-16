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

use alloc::collections::BTreeMap;
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

use super::recording::{Fold, LevelTrack, Verb};
use super::scene::{PositionBadge, RecordingBar};
use super::strings::{Arg, Lang, StrId, Text};
use super::{leg_label, plain, push, reactions, wrap, Conversation, Message, Signature, UiModel};
use crate::telemetry;
use crate::when::{date_label, WallTime};

/// What an empty conversation says (1447, walk 1 finding 22): "Noch keine
/// Nachrichten", where it said "nothing earlier on this device", a sentence
/// about the store and in English; over messages the top row is blank.
pub const EARLIEST: StrId = StrId::NoMessagesYet;

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
    /// The voice messages' level tracks the model holds (1276): a
    /// bubble's waveform is its track's where there is one.
    pub tracks: Option<&'a BTreeMap<([u8; 16], u64), LevelTrack>>,
    /// The reader's language, for the pane's own words (1447).
    pub lang: Lang,
    /// The reader's names, for a relayed post's writer (1482); none draws
    /// a relay's posts as they came.
    pub names: Option<&'a crate::names::Names>,
    /// The bubbles' bytes under way (1662); none draws no bar.
    pub transfers: Option<&'a super::transfer::Transfers>,
    /// The writer rule (the audits of 1757 and 1787): whose position a
    /// bubble's line names, and none for a claim nobody vouches for.
    pub writers: super::Writers<'a>,
}

/// The cells of a bubble's progress bar (1662).
pub const PROGRESS_CELLS: usize = 10;

/// `percent` of `cells` filled: `▰▰▰▰▱▱▱▱▱▱`, `[####------]` in ASCII.
pub fn progress_bar(percent: u8, cells: usize, ascii: bool) -> String {
    let filled = (usize::from(percent.min(100)) * cells)
        .div_ceil(100)
        .min(cells);
    let (on, off) = match ascii {
        true => ('#', '-'),
        false => ('▰', '▱'),
    };
    let bar: String = core::iter::repeat_n(on, filled)
        .chain(core::iter::repeat_n(off, cells - filled))
        .collect();
    match ascii {
        true => alloc::format!("[{bar}]"),
        false => bar,
    }
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
    /// The block's own rows among them, absolute: where they start and
    /// how many, without the separators above and the reaction row
    /// under. What a selection change re-lays ([`respan`], 1490).
    pub block: (usize, usize),
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
#[derive(Clone, Debug)]
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
    /// No conversation open, or one without messages: the pane says that
    /// there are none yet.
    pub fn empty(width: usize, tokens: &Tokens, lang: Lang) -> Self {
        Self {
            rows: vec![centred(EARLIEST.text(lang), width, painted(tokens.muted))],
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
    let mut laid = Laid::empty(width, draw.tokens, draw.lang);
    if conversation.earlier > 0 {
        laid.rows[0] = centred(
            &earlier_line(conversation.earlier),
            width,
            painted(draw.tokens.muted),
        );
    } else if !conversation.messages.is_empty() {
        laid.rows[0] = centred("", width, painted(draw.tokens.muted));
    }
    let mut last = Last::Separator;
    let mut day = None;
    // The first of the reactions and reports folded since the last block: a
    // note or the `new` rule that stood before one goes before the next
    // block instead.
    let mut skipped: Option<usize> = None;
    let reactions = reactions::gather(conversation);
    // A relay's chat (1482): each post names its writer in its line.
    let relayed = draw
        .names
        .filter(|_| super::relayed::is_relayed(conversation));
    for (index, message) in conversation.messages.iter().enumerate() {
        let start = laid.rows.len();
        if folded(conversation, index, relayed.is_some()) {
            skipped.get_or_insert(index);
            // Drawn as the report after it: no rows, so no click lands on
            // it and the cursor steps over it.
            laid.spans.push(Span {
                start,
                end: start,
                mark: None,
                block: (start, 0),
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
        // A group's membership that changed somebody's standing (1307 G2):
        // a line of its own, as a note is.
        if let Some(text) =
            super::groups::membership_note(&conversation.messages, index, None, Lang::En)
        {
            if last == Last::Block {
                laid.push(RLine::default());
            }
            laid.push(note_line(&text, width, draw.tokens));
            laid.spans.push(Span {
                start,
                end: laid.rows.len(),
                mark: None,
                block: (start, 0),
            });
            last = Last::Separator;
            continue;
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
            relayed,
            &mut laid.links,
        );
        let block_rows = drawn.rows.len();
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
            block: (first, block_rows),
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

/// Re-lay message `index`'s block inside `laid` after only the
/// selection changed (1490): the same rows in place, restyled, the
/// separators and the reaction row untouched. True when the splice
/// held; false (a block whose row count moved with the selection, which
/// none does today) asks the caller for a fresh layout. New link cells
/// point at entries appended to `laid.links`; the old entries stay, so
/// every index that was handed out keeps its meaning.
pub fn respan(
    laid: &mut Laid,
    conversation: &Conversation,
    index: usize,
    width: usize,
    draw: &Draw<'_>,
    selected: bool,
) -> bool {
    let width = width.max(1);
    let Some(span) = laid.spans.get(index).copied() else {
        return false;
    };
    if span.block.1 == 0 {
        // A folded report or a note line: nothing a selection restyles.
        return true;
    }
    let relayed = draw
        .names
        .filter(|_| super::relayed::is_relayed(conversation));
    let drawn = block(
        conversation,
        index,
        width,
        draw,
        selected,
        relayed,
        &mut laid.links,
    );
    let (at, n) = span.block;
    if drawn.rows.len() != n {
        return false;
    }
    laid.rows.splice(at..at + n, drawn.rows);
    laid.texts.splice(at..at + n, drawn.texts);
    laid.spans[index].mark = drawn.mark.map(|(row, col, len)| (at + row, col, len));
    true
}

/// Whether message `index` has no block of its own: a reaction, drawn
/// under its target ([`reactions`]), or a telemetry report the next one
/// replaces on screen: one peer's consecutive reports fold into the latest
/// (`loquax-map.md`, "In the conversation"), by the rule every screen folds
/// by ([`Conversation::report_folded`]). In a relay's chat (`relayed`,
/// 1482) a reaction whose target is not here has a block, in words.
pub fn folded(conversation: &Conversation, index: usize, relayed: bool) -> bool {
    conversation
        .messages
        .get(index)
        .is_some_and(Message::is_aside)
        && super::groups::membership_note(&conversation.messages, index, None, Lang::En).is_none()
        && !(relayed
            && conversation
                .messages
                .get(index)
                .and_then(|m| super::relayed::orphan_reaction(conversation, m))
                .is_some())
        || conversation.report_folded(index)
}

/// `── was Alice, now announces Ally ──`, centred and muted: a peer's name
/// changed, said once where it happened (`loquax-mailbox.md`, the naming
/// decision, 2). Cut to the pane with `…` when it is wider. The M9's chat
/// draws a group's own lines with it (1356).
pub fn note_line(text: &str, width: usize, tokens: &Tokens) -> RLine {
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
/// The M9's chat draws its day lines with it (1356).
pub fn date_line(label: &str, width: usize, tokens: &Tokens) -> RLine {
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
    relayed: Option<&crate::names::Names>,
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
            // The scene's one sentence (1449 item 5, walk 1 finding 11):
            // `Live position` while the newest report is alive, else when
            // the fix was taken; the coordinates are the Info's.
            match draw.writers.fix(peer, message) {
                Some(fix) => {
                    let now = draw.now.map(|now| now.unix_secs);
                    let alive = conversation.newest_fix_report() == Some(index)
                        && conversation.liveness(now).state == super::liveness::State::Live;
                    format!(
                        "{} {}",
                        telemetry::REPORT_MARK,
                        super::scene::position_words(&fix, alive, draw.now).resolve(draw.lang)
                    )
                }
                None => {
                    let received = message.at.map(|at| at.unix_secs);
                    telemetry::report_line(&draw.writers.read(peer, &message.fields, received))
                }
            }
        }
        (false, mark) => {
            // A group's copy without words names its writer as every copy
            // does here, not with the home's sentence (1388).
            let body = super::groups::stand_in_line(&message.body, &message.fields)
                .or_else(|| {
                    relayed.and_then(|names| {
                        super::relayed::terminal_line(names, draw.lang, conversation, message)
                    })
                })
                .unwrap_or_else(|| message.body.clone());
            match mark {
                Some(query) => super::search::mark_first(&body, query),
                None => body,
            }
        }
    };
    // A text that came with a position says so under it, in a report's
    // words: `Enter` on it opens the map there.
    let body = match (message.is_report(), draw.writers.fix(peer, message)) {
        (false, Some(fix)) => format!(
            "{body}\n{} {}",
            telemetry::REPORT_MARK,
            super::scene::position_words(&fix, false, draw.now).resolve(draw.lang)
        ),
        _ => body,
    };
    // The media first, the words under them (1268), as every target draws
    // a bubble: a photo (1220) says so, `Enter` on it shows it whole in
    // the system's viewer; a file (1221) its name and size, `Enter` saves
    // it under the home and opens it; a voice message (1222) its length,
    // `Enter` plays it. Without words the media is the block alone, no
    // empty row under it.
    let (dot, photo_glyph, file_glyph, voice_glyph) = match draw.ascii {
        true => ("-", "[#]", "[f]", "[v]"),
        false => ("·", "▣", "▤", "♪"),
    };
    let mut media = Vec::new();
    if let Some(photo) = super::photos::photo_of(&message.fields) {
        media.push(format!(
            "{photo_glyph} photo {dot} {}",
            super::photos::size_words(photo.bytes as u64)
        ));
    }
    for file in super::files::files_of(&message.fields) {
        media.push(format!(
            "{file_glyph} {} {dot} {}",
            super::files::safe_name(&file.name),
            super::photos::size_words(file.bytes as u64)
        ));
    }
    if let Some(voice) = super::voice::voice_of(&message.fields) {
        media.push(format!(
            "{voice_glyph} voice message {dot} {}",
            super::voice::duration_words(voice.duration_ms)
        ));
        // Its waveform under it (1276), the scene's bars in block
        // characters: the recording's own track or the decoded sound's
        // where the model has one, the stream's packet sizes otherwise.
        let track = draw
            .tracks
            .and_then(|tracks| tracks.get(&(peer, message.activity)));
        let levels = match track {
            Some(track) => track.bars(super::voice::BARS, Fold::Peak),
            None => voice.levels.clone(),
        };
        if !levels.is_empty() {
            let n = super::voice::BARS.min(cap).max(1);
            media.push(waveform(&levels, n, false, draw.ascii));
        }
    }
    // Its bytes under way (1662), a bar and its words under the media:
    // ours while it still sends, one being fetched until it is whole.
    let sending = |state: Option<leviculum_ui::delivery::DeliveryState>| {
        let shown = state.map(|s| super::shown_delivery(s, draw.home_client, draw.receipts));
        super::chat::Mark::of(shown) == super::chat::Mark::Sending
    };
    if let Some(transfer) = draw
        .transfers
        .and_then(|transfers| transfers.of(&peer, message.activity))
        .filter(|_| !message.outbound || sending(message.delivery))
    {
        media.push(format!(
            "{} {}",
            progress_bar(transfer.percent(), PROGRESS_CELLS, draw.ascii),
            transfer.words().resolve(draw.lang)
        ));
    }
    if !body.trim().is_empty() {
        media.push(body);
    }
    let body = media.join("\n");
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
    // Only what the pane shows is laid (1490): the entries on screen and
    // the one the bottom may cut.
    let until = (first + entries_shown(height) + 1).min(order.len());
    for &index in &order[first..until] {
        let conversation = &model.conversations[index];
        let here = index == model.selected;
        let [top, bottom] = entry(model, conversation, width, here);
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
    // Every row laid, the ones cut off below too (1489).
    model.count(super::counters::Op::RowsLaid, rows.len());
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
/// instead of the last line, `Draft: …` in italics (1219). The words are
/// the reader's language's, the last line the one the phone and the window
/// show ([`super::chat::list_row`], 1381): an invitation says who invited
/// to what, never the wire's English with its code.
fn entry<B>(
    model: &UiModel<B>,
    conversation: &Conversation,
    width: usize,
    open: bool,
) -> [RLine; 2] {
    let (names, now, lang) = (&model.names, model.now, model.lang);
    let room = width.saturating_sub(2);
    let last = conversation
        .messages
        .iter()
        .rev()
        .find(|m| !m.is_group_state());
    // Nothing loaded: the store's row carries the time and the line
    // (1490, schema 17), the same row the phone and the M9 draw.
    let listing = conversation
        .messages
        .is_empty()
        .then_some(conversation.listing.as_ref())
        .flatten();
    let time = match (last.and_then(|m| m.at).or(listing.and_then(|l| l.at)), now) {
        (Some(at), Some(now)) => super::scene::list_time(&at, &now).resolve(lang),
        _ => String::new(),
    };
    let mut top = RLine::default();
    push(&mut top, " ", RStyle::default());
    let title = names.label_in(
        &conversation.peer,
        room.saturating_sub(time.chars().count() + 1),
        lang,
    );
    // Two peers of one name (1452): the sentence after the name, muted,
    // where the time leaves room for all of it; a cut sentence is noise,
    // and the header says it whole.
    let free = room.saturating_sub(fit_width(&title) + time.chars().count() + 1);
    let same = names
        .same_name(&conversation.peer)
        .map(|sentence| format!("  {}", sentence.resolve(lang)))
        .filter(|same| fit_width(same) <= free)
        .unwrap_or_default();
    let gap = room.saturating_sub(fit_width(&title) + fit_width(&same) + time.chars().count());
    push(&mut top, &title, RStyle::default());
    push(&mut top, &same, painted(model.tokens().muted));
    push(
        &mut top,
        &format!("{}{time} ", " ".repeat(gap)),
        RStyle::default(),
    );

    let badge = match conversation.unread_rows() {
        0 => String::new(),
        n => format!("[{n}]"),
    };
    let preview = match last {
        // Whether the position is alive, not how many came (1209).
        Some(message) if message.is_report() => format!(
            "{} position · {}",
            telemetry::REPORT_MARK,
            conversation
                .liveness(now.map(|now| now.unix_secs))
                .list_words(now.map(|now| now.unix_secs))
        ),
        Some(message) => {
            let row = super::chat::list_row(model, &conversation.peer);
            let first = row.last.split('\n').next().unwrap_or("");
            // A reaction and an invitation say who did it themselves.
            let invite = leviculum_lxmf::lev::Lev::from_fields(&message.fields)
                .is_some_and(|lev| lev.invite.is_some());
            match message.outbound && !message.is_reaction() && !invite {
                true => Text::Key(StrId::RowYou, vec![Arg::Str(first.into())]).resolve(lang),
                false => first.into(),
            }
        }
        // Nothing loaded: the store's row, through [`list_row`]'s listing
        // path, worded as the loaded one is (1490).
        None if listing.is_some() => {
            let row = super::chat::list_row(model, &conversation.peer);
            let first = String::from(row.last.split('\n').next().unwrap_or(""));
            match row.ours && row.kind != "reaction" {
                true => Text::Key(StrId::RowYou, vec![Arg::Str(first)]).resolve(lang),
                false => first,
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
    .map(|_| format!("{} ", StrId::PositionMarker.text(lang)))
    .unwrap_or_default();
    let marks = pin.chars().count() + badge.chars().count();
    let budget = match marks {
        0 => room,
        n => room.saturating_sub(n + 1),
    };
    let draft = conversation.draft.line().filter(|_| !open);
    let preview = match &draft {
        Some(line) => Text::Key(StrId::DraftRow, vec![Arg::Str(line.clone())]).resolve(lang),
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

/// The eight steps of a level, 0 to 100, as block characters: loud is
/// tall (1276).
pub const LEVEL_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The same eight steps where the screen draws ASCII alone.
pub const LEVEL_ASCII: [char; 8] = ['_', '.', '-', ':', '=', '+', '*', '#'];

/// A level's character: one of the eight steps.
pub fn level_glyph(level: u8, ascii: bool) -> char {
    let step = usize::from(level.min(100)) * 8 / 101;
    match ascii {
        true => LEVEL_ASCII[step],
        false => LEVEL_BLOCKS[step],
    }
}

/// `levels` drawn `width` cells wide. Moving (it records), the newest
/// stand at the right edge and a short track is blank to the left of
/// them; standing, the whole track is folded to the width by the one
/// resampler ([`LevelTrack::bars`]).
pub fn waveform(levels: &[u8], width: usize, moving: bool, ascii: bool) -> String {
    let shown: Vec<u8> = match moving {
        true => levels[levels.len().saturating_sub(width)..].to_vec(),
        false => LevelTrack::of(levels.to_vec()).bars(width, Fold::Peak),
    };
    let blank = width.saturating_sub(shown.len());
    let mut out: String = core::iter::repeat_n(' ', blank).collect();
    out.extend(shown.iter().map(|&l| level_glyph(l, ascii)));
    out
}

/// The terminal's words for a recording bar's button (1276): the key
/// and what it does. Send's key is the live send chord's, which `v` and
/// `Alt-v` stand beside.
pub fn rec_key_words(verb: Verb, send: &str) -> Option<String> {
    let words = match verb {
        Verb::Play => "Space play",
        Verb::StopPlay => "Space stop",
        Verb::Pause => "p pause",
        Verb::Resume => "p resume",
        Verb::Discard => "Esc cancel",
        Verb::Send => return Some(format!("{send} send")),
        Verb::Lock | Verb::AskMicrophone => return None,
    };
    Some(words.into())
}

/// The silence line's button, after its words: a click says how to let
/// the microphone through.
pub const REC_ALLOW: &str = "allow";

/// The recording bar as the terminal draws it in the compose box, and
/// where its buttons are.
#[derive(Clone, Debug)]
pub struct RecRows {
    /// The bar's row, then the line under the waveform when it says
    /// something.
    pub rows: Vec<RLine>,
    /// The buttons' cells: a row, its first column, how many columns, and
    /// the verb a click raises.
    pub buttons: Vec<(usize, usize, usize, Verb)>,
}

/// The order the terminal names the bar's buttons in: listening first,
/// sending last, where the eye ends.
fn rec_order(verb: Verb) -> u8 {
    match verb {
        Verb::Play | Verb::StopPlay => 0,
        Verb::Pause | Verb::Resume => 1,
        Verb::Discard => 2,
        Verb::Send => 3,
        Verb::Lock | Verb::AskMicrophone => 4,
    }
}

/// `bar` in the compose box, `width` cells inside its border (1276):
/// `● REC 00:07  ▁▂▃▅▇…  p pause · Esc cancel · Enter send`, the dot and
/// the timer in the theme's red (the timer amber in the last 30 s), the
/// dot hollow once it stands and dimmed every other half second while it
/// pulses, the waveform as block characters, the keys muted; under it the
/// line the scene says, in amber, its click where it has one. `send` is
/// the live send chord, `tick` the model's quarter seconds.
pub fn recording_rows(
    bar: &RecordingBar,
    width: usize,
    ascii: bool,
    tokens: &Tokens,
    send: &str,
    tick: u64,
) -> RecRows {
    let red = RStyle {
        bold: true,
        ..painted(tokens.error)
    };
    // No amber among the tokens: the terminal's 214 where it draws
    // colour, which the renderer brings down to the tier's palette.
    let amber = RStyle {
        fg: tokens
            .error
            .fg
            .map(|_| leviculum_ui::color::Ink::Rgb(0xff, 0xaf, 0x00)),
        bold: true,
        ..RStyle::default()
    };
    let muted = painted(tokens.muted);
    let running = bar.phase.running();
    let dot = match (running, ascii) {
        (true, false) => "●",
        (false, false) => "○",
        (true, true) => "*",
        (false, true) => "o",
    };
    let dot_style = match bar.pulsing && (tick / 2) % 2 == 1 {
        true => muted,
        false => red,
    };
    let timer = bar.timer.words.resolve(Lang::En);
    let mut buttons: Vec<Verb> = bar.buttons.iter().map(|b| b.verb).collect();
    buttons.sort_by_key(|&v| rec_order(v));
    let mut keys: Vec<(Verb, String)> = buttons
        .into_iter()
        .filter_map(|v| rec_key_words(v, send).map(|w| (v, w)))
        .collect();
    let sep = match ascii {
        true => " - ",
        false => " · ",
    };
    let head = 1 + 1 + timer.chars().count() + 2;
    let keys_len = |keys: &[(Verb, String)]| -> usize {
        match keys.len() {
            0 => 0,
            n => 2 + keys.iter().map(|(_, w)| w.chars().count()).sum::<usize>() + 3 * (n - 1),
        }
    };
    // Where the screen is narrow the keys go, `Esc cancel` first (the
    // border names it too), then listening, then pausing, Send never: the
    // waveform keeps eight cells.
    for verb in [
        Verb::Discard,
        Verb::Play,
        Verb::StopPlay,
        Verb::Pause,
        Verb::Resume,
    ] {
        if head + 8 + keys_len(&keys) <= width {
            break;
        }
        keys.retain(|(v, _)| *v != verb);
    }
    let wave_width = width
        .saturating_sub(head + keys_len(&keys))
        .min(super::recording::LIVE_WINDOWS);
    let mut row = RLine::default();
    push(&mut row, dot, dot_style);
    push(&mut row, " ", RStyle::default());
    push(
        &mut row,
        &timer,
        match bar.timer.amber {
            true => amber,
            false => red,
        },
    );
    push(&mut row, "  ", RStyle::default());
    push(
        &mut row,
        &waveform(&bar.wave.levels, wave_width, bar.wave.moving, ascii),
        RStyle::default(),
    );
    let mut hits = Vec::new();
    if !keys.is_empty() {
        push(&mut row, "  ", RStyle::default());
        for (n, (verb, words)) in keys.iter().enumerate() {
            if n > 0 {
                push(&mut row, sep, muted);
            }
            hits.push((0, row.cells.len(), words.chars().count(), *verb));
            push(&mut row, words, muted);
        }
    }
    row.cells.truncate(width);
    let mut rows = vec![row];
    if let Some(notice) = &bar.notice {
        let mut line = RLine::default();
        push(&mut line, &notice.words.resolve(Lang::En), amber);
        if notice.action.is_some() {
            push(&mut line, sep, muted);
            hits.push((
                1,
                line.cells.len(),
                REC_ALLOW.chars().count(),
                Verb::AskMicrophone,
            ));
            push(&mut line, REC_ALLOW, muted);
        }
        line.cells.truncate(width);
        rows.push(line);
    }
    hits.retain(|&(_, col, _, _)| col < width);
    RecRows {
        rows,
        buttons: hits,
    }
}
