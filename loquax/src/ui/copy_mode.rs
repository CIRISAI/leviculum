//! The terminal's copy mode (1475): `v` (vi) or `Ctrl-Space` (the Emacs
//! mark) in the messages puts a cursor on the open conversation's text,
//! the selected message's first character, and selects from there to
//! wherever the cursor goes, as vi's visual mode and Emacs's region do.
//! The arrows, `h` `j` `k` `l`, `w` `b`, `0` `$` (and `Ctrl-f` `Ctrl-b`
//! `Ctrl-n` `Ctrl-p`, `Alt-f` `Alt-b`, `Ctrl-a` `Ctrl-e`) move it, Shift
//! with an arrow too; `y`, `Alt-w` or `Enter` copy what is selected (OSC
//! 52, as a drag's release does) and leave; `Esc`, `Ctrl-g`, `q` or `v`
//! leave without a copy.
//!
//! The selection is the core's ([`super::selection`]): each message with
//! text is a run, its text the cells the layout drew for it, its rows
//! joined as [`super::hit::selected_text`] joins them (a wrapped row by
//! the space the wrap took, a body's lines by a newline). The cursor is a
//! cell, [`spot_of`] turns it into a [`Spot`] of the run, and
//! [`TextSelection::extend`] and [`TextSelection::text`] give the copy, one
//! line a message with its writer and time across messages, as the
//! window's. The cells drawn as selected are the two cells as a drag's
//! [`super::Selection`], which the layout already draws; they are the
//! characters the core selection covers ([`selection`], and the test
//! that holds the two to each other).

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::input::{InputBuffer, KeyCode, KeyEvent, KeyModifiers};

use super::layout::Laid;
use super::selection::{word_at, RunKey, Spot, TextRun, TextSelection};
use super::strings::StrId;
use super::{Selection, ToastKind, UiEffect, UiModel};

/// Where the copy mode's cursor is and where its selection began, each a
/// laid-out row and a column of the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyMode {
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
}

/// A text row of a run: the laid row, its first column, its cells and the
/// byte of the run's text its first cell is.
#[derive(Clone, Copy, Debug)]
struct Piece {
    row: usize,
    start: usize,
    len: usize,
    byte: usize,
}

/// The conversation's runs as the layout drew them, and each one's rows.
pub struct Runs {
    pub runs: Vec<TextRun>,
    pieces: Vec<Vec<Piece>>,
}

/// The open conversation's text runs from its layout: one for each
/// message with text rows, in order.
pub fn runs<B: InputBuffer>(model: &UiModel<B>, laid: &Laid) -> Runs {
    let messages = model.messages_of_selected();
    let peer = model.conversations.get(model.selected).map(|c| c.peer);
    let mut runs: Vec<TextRun> = Vec::new();
    let mut pieces: Vec<Vec<Piece>> = Vec::new();
    let mut last: Option<usize> = None;
    for (row, text) in laid.texts.iter().enumerate() {
        let Some(text) = text else {
            continue;
        };
        let Some(message) = messages.get(text.message) else {
            continue;
        };
        if last != Some(text.message) {
            let who = match (message.outbound, peer) {
                (true, _) => String::from(StrId::QuoteYou.text(model.lang)),
                (false, Some(peer)) => model.names.label(&peer, model.lang),
                (false, None) => String::new(),
            };
            runs.push(TextRun {
                key: RunKey::body(message.activity),
                text: String::new(),
                who,
                at: message.at.map(|at| at.hh_mm()),
            });
            pieces.push(Vec::new());
            last = Some(text.message);
        } else if let Some(run) = runs.last_mut() {
            run.text.push(match text.wrapped {
                true => ' ',
                false => '\n',
            });
        }
        let (Some(run), Some(rows)) = (runs.last_mut(), pieces.last_mut()) else {
            continue;
        };
        rows.push(Piece {
            row,
            start: text.start,
            len: text.len,
            byte: run.text.len(),
        });
        let cells = laid.rows[row].cells.get(text.start..text.start + text.len);
        run.text.extend(cells.into_iter().flatten().map(|c| c.ch));
    }
    Runs { runs, pieces }
}

impl Runs {
    /// The run and the piece a laid row is a text row of.
    fn piece(&self, row: usize) -> Option<(usize, Piece)> {
        self.pieces
            .iter()
            .enumerate()
            .find_map(|(n, rows)| rows.iter().find(|p| p.row == row).map(|p| (n, *p)))
    }

    /// The text rows in order, as (run, piece).
    fn rows(&self) -> impl Iterator<Item = (usize, Piece)> + '_ {
        (self.pieces.iter().enumerate()).flat_map(|(n, rows)| rows.iter().map(move |p| (n, *p)))
    }
}

/// The place in its run of the cell `cell`, before the cell's character:
/// the column held to its row's text. None off a text row.
pub fn spot_of(runs: &Runs, laid: &Laid, cell: (usize, usize)) -> Option<Spot> {
    let (n, piece) = runs.piece(cell.0)?;
    let col = cell.1.clamp(piece.start, piece.start + piece.len);
    let before: usize = (laid.rows[cell.0].cells.get(piece.start..col))
        .into_iter()
        .flatten()
        .map(|c| c.ch.len_utf8())
        .sum();
    Some(Spot {
        key: runs.runs[n].key,
        at: piece.byte + before,
    })
}

/// The cell a spot is on: the row whose text holds its byte, the column of
/// the character there. None for a spot in no run.
pub fn cell_of(runs: &Runs, laid: &Laid, spot: Spot) -> Option<(usize, usize)> {
    let n = runs.runs.iter().position(|r| r.key == spot.key)?;
    let rows = &runs.pieces[n];
    // The last row that begins at or before the byte.
    let piece = rows
        .iter()
        .rev()
        .find(|p| p.byte <= spot.at)
        .or(rows.first())?;
    let mut byte = piece.byte;
    let mut col = piece.start;
    for c in (laid.rows[piece.row]
        .cells
        .get(piece.start..piece.start + piece.len))
    .into_iter()
    .flatten()
    {
        if byte >= spot.at {
            break;
        }
        byte += c.ch.len_utf8();
        col += 1;
    }
    Some((piece.row, col))
}

/// The core selection the copy mode stands for: from the first of its two
/// cells to after the last one's character, both in.
pub fn selection(runs: &Runs, laid: &Laid, mode: &CopyMode) -> Option<TextSelection> {
    let (from, to) = match mode.anchor <= mode.cursor {
        true => (mode.anchor, mode.cursor),
        false => (mode.cursor, mode.anchor),
    };
    let anchor = spot_of(runs, laid, from)?;
    let head = spot_of(runs, laid, (to.0, to.1 + 1))?;
    Some(TextSelection::caret(anchor).extend(head))
}

/// The open conversation laid out at the pane's width, as the copy mode
/// and the screen see it.
pub fn laid<B: InputBuffer>(model: &UiModel<B>) -> alloc::sync::Arc<Laid> {
    model.laid()
}

/// The copy mode on: the cursor on the selected message's first text cell.
/// Nothing when it has none (a folded report, an empty chat).
pub fn enter<B: InputBuffer>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let laid = model.laid();
    let runs = runs(model, &laid);
    let first = (laid.spans.get(model.message_cursor))
        .and_then(|span| (span.start..span.end).find_map(|row| runs.piece(row)))
        .or_else(|| runs.rows().next());
    let Some((_, piece)) = first else {
        model.toast(ToastKind::Info, String::from("nothing to select"));
        return Vec::new();
    };
    let at = (piece.row, piece.start);
    model.copy_mode = Some(CopyMode {
        anchor: at,
        cursor: at,
    });
    show(model, &laid);
    Vec::new()
}

/// A key in the copy mode.
pub fn key<B: InputBuffer>(model: &mut UiModel<B>, key: KeyEvent) -> Vec<UiEffect> {
    let Some(mut mode) = model.copy_mode else {
        return Vec::new();
    };
    let laid = model.laid();
    let runs = runs(model, &laid);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let step =
        match (key.code, ctrl, alt) {
            (KeyCode::Char('y'), false, false)
            | (KeyCode::Char('w'), false, true)
            | (KeyCode::Enter, ..) => {
                let text = selection(&runs, &laid, &mode)
                    .map(|s| s.text(&runs.runs))
                    .unwrap_or_default();
                leave(model);
                return super::copy_text(model, text);
            }
            (KeyCode::Esc, ..)
            | (KeyCode::Char('g'), true, _)
            | (KeyCode::Char('q' | 'v'), false, false) => {
                leave(model);
                return Vec::new();
            }
            (KeyCode::Left | KeyCode::Char('h'), false, false)
            | (KeyCode::Char('b'), true, false) => Move::Char(false),
            (KeyCode::Right | KeyCode::Char('l'), false, false)
            | (KeyCode::Char('f'), true, false) => Move::Char(true),
            (KeyCode::Up | KeyCode::Char('k'), false, false)
            | (KeyCode::Char('p'), true, false) => Move::Row(false),
            (KeyCode::Down | KeyCode::Char('j'), false, false)
            | (KeyCode::Char('n'), true, false) => Move::Row(true),
            (KeyCode::Char('w'), false, false) | (KeyCode::Char('f'), false, true) => {
                Move::Word(true)
            }
            (KeyCode::Char('b'), false, false) | (KeyCode::Char('b'), false, true) => {
                Move::Word(false)
            }
            (KeyCode::Char('0') | KeyCode::Home, false, false)
            | (KeyCode::Char('a'), true, false) => Move::Edge(false),
            (KeyCode::Char('$') | KeyCode::End, false, false)
            | (KeyCode::Char('e'), true, false) => Move::Edge(true),
            _ => return Vec::new(),
        };
    if let Some(cell) = moved(&runs, &laid, mode.cursor, step) {
        mode.cursor = cell;
    }
    model.copy_mode = Some(mode);
    show(model, &laid);
    Vec::new()
}

/// The copy mode off, its selection gone from the screen.
pub fn leave<B>(model: &mut UiModel<B>) {
    model.copy_mode = None;
    model.selection = None;
}

/// How the cursor moves.
#[derive(Clone, Copy)]
enum Move {
    /// A character on (`true`) or back, over a row's end to the next.
    Char(bool),
    /// A text row down or up, the column held to it.
    Row(bool),
    /// To the next word's start, or the start of this or the last one.
    Word(bool),
    /// The row's end (`true`) or its start.
    Edge(bool),
}

fn moved(runs: &Runs, laid: &Laid, at: (usize, usize), step: Move) -> Option<(usize, usize)> {
    let rows: Vec<(usize, Piece)> = runs.rows().collect();
    let here = rows.iter().position(|(_, p)| p.row == at.0)?;
    let piece = rows[here].1;
    let last_col = |p: &Piece| p.start + p.len.saturating_sub(1);
    match step {
        Move::Char(true) if at.1 < last_col(&piece) => Some((at.0, at.1 + 1)),
        Move::Char(true) => rows.get(here + 1).map(|(_, p)| (p.row, p.start)),
        Move::Char(false) if at.1 > piece.start => Some((at.0, at.1 - 1)),
        Move::Char(false) => here
            .checked_sub(1)
            .map(|n| (rows[n].1.row, last_col(&rows[n].1))),
        Move::Row(down) => {
            let next = match down {
                true => rows.get(here + 1),
                false => here.checked_sub(1).map(|n| &rows[n]),
            }?;
            Some((next.1.row, at.1.clamp(next.1.start, last_col(&next.1))))
        }
        Move::Edge(true) => Some((at.0, last_col(&piece))),
        Move::Edge(false) => Some((at.0, piece.start)),
        Move::Word(forward) => {
            let spot = spot_of(runs, laid, at)?;
            let n = runs.runs.iter().position(|r| r.key == spot.key)?;
            let text = &runs.runs[n].text;
            let to = match forward {
                true => next_word(text, spot.at),
                false => last_word(text, spot.at),
            };
            match to {
                Some(byte) => cell_of(runs, laid, Spot { at: byte, ..spot }),
                // Past the run's last word: the next run's first, or the
                // last one's last.
                None if forward => (runs.runs.get(n + 1))
                    .and_then(|r| cell_of(runs, laid, Spot { key: r.key, at: 0 })),
                None => (n.checked_sub(1)).and_then(|m| {
                    let run = &runs.runs[m];
                    let at = last_word(&run.text, run.text.len()).unwrap_or(0);
                    cell_of(runs, laid, Spot { key: run.key, at })
                }),
            }
        }
    }
}

/// The start of the word after the one at `at`, blanks skipped; none at
/// the text's last word.
fn next_word(text: &str, at: usize) -> Option<usize> {
    let mut i = word_at(text, at).end.max(at + 1).min(text.len());
    while i < text.len() {
        let c = text[i..].chars().next()?;
        if !c.is_whitespace() {
            return Some(i);
        }
        i += c.len_utf8();
    }
    None
}

/// The start of the word before `at` (of the one `at` is inside, when it
/// is not at its start); none at the text's first.
fn last_word(text: &str, at: usize) -> Option<usize> {
    let mut i = super::selection::clamp(text, at);
    // Back over the blanks before.
    loop {
        let (j, c) = text[..i].char_indices().next_back()?;
        if !c.is_whitespace() {
            return Some(word_at(text, j).start);
        }
        i = j;
    }
}

/// The selection drawn as the drag's, and the cursor's row on screen.
fn show<B: InputBuffer>(model: &mut UiModel<B>, laid: &Laid) {
    let Some(mode) = model.copy_mode else {
        return;
    };
    // The two cells in reading order, both in, as a drag's: the core
    // selection covers the same characters ([`selection`]).
    let drawn = Selection {
        anchor: mode.anchor.min(mode.cursor),
        head: mode.anchor.max(mode.cursor),
    };
    model.selection = Some(drawn);
    // The pane follows the cursor.
    let rows = model.message_rows().max(1);
    let total = laid.rows.len();
    let mut top = model.top_of(total);
    let row = mode.cursor.0;
    if row >= top + rows {
        top = row + 1 - rows;
    }
    if row < top {
        top = row;
    }
    model.scroll_top = Some(top);
}
