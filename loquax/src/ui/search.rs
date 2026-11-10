//! The search (`docs/src/concepts/loquax-keys-and-layout.md`, section 5,
//! "Search", and section 7, slice 4): what a query finds in this session's
//! conversations, and the list pane it turns into.
//!
//! The matcher is a case-insensitive substring test over the conversation
//! names as the screen shows them ([`crate::names::Names::label`]: the name
//! and its short hash, or the address while a peer has no name) and the
//! message bodies. [`hits`] lists what it finds in two groups,
//! the conversations first and then the messages, each most recent first;
//! a message whose conversation also matches by name is in both. The
//! matcher searches what the screen has loaded; what the store holds
//! further back its FTS5 index finds ([`Hit::Stored`]), and those join the
//! messages group by their age.
//!
//! [`rows`] lays the results out as the list pane shows them under the
//! field: a group header with its count, then two rows a result, who and
//! when over the preview line with the first match between `«` and `»`.
//! The conversation pane marks the match in place the same way
//! ([`mark_first`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::text::{fit_width, truncate_to_cols};
use leviculum_ui::{RLine, RStyle};

use super::history::StoredHit;
use super::layout::{painted, BAR};
use super::{bold, push, Conversation, UiModel};
use crate::names::Names;
use crate::when::WallTime;

/// The mark before a match.
pub const OPEN: char = '«';
/// The mark after it.
pub const CLOSE: char = '»';

/// What the list says when the query finds nothing.
pub const NO_MATCHES: &str = "no matches";

/// The group header over the conversations that match by name.
pub const CONVERSATIONS: &str = "CONVERSATIONS";
/// The group header over the messages whose text matches.
pub const MESSAGES: &str = "MESSAGES";

/// Rows the field takes at the top of the list pane: the field and a blank.
pub const FIELD_ROWS: usize = 2;

/// The open search: the field, and the result the cursor is on.
#[derive(Clone, Debug, Default)]
pub struct Search<B> {
    pub query: B,
    /// The selected result, an index into [`hits`].
    pub selected: usize,
    /// What the store found for the query below what the screen loaded.
    pub stored: Vec<StoredHit>,
}

/// One result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A conversation whose name matches, by its index.
    Conversation(usize),
    /// A message whose text matches.
    Message { conversation: usize, message: usize },
    /// A message the store found that the screen has not loaded, by its
    /// index in [`Search::stored`].
    Stored(usize),
}

/// Where `needle` first occurs in `hay`, ignoring case: its first
/// character's index and its length, both in characters. An empty needle
/// is found nowhere.
pub fn find(hay: &str, needle: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let hay: Vec<char> = hay.chars().collect();
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    (0..=hay.len().checked_sub(needle.len())?)
        .find(|&at| {
            hay[at..at + needle.len()]
                .iter()
                .zip(&needle)
                .all(|(&a, &b)| same(a, b))
        })
        .map(|at| (at, needle.len()))
}

/// `text` with its first match of `query` between `«` and `»`, or as it is
/// when there is none.
pub fn mark_first(text: &str, query: &str) -> String {
    let Some((at, len)) = find(text, query) else {
        return String::from(text);
    };
    let mut out = String::with_capacity(text.len() + 4);
    for (i, c) in text.chars().enumerate() {
        if i == at {
            out.push(OPEN);
        }
        out.push(c);
        if i + 1 == at + len {
            out.push(CLOSE);
        }
    }
    out
}

/// What `query` finds among the `conversations` the list shows: those whose
/// name matches, in `order` (the list's, most recent first), then every
/// message of theirs whose text matches, `stored` among them, the most
/// Whether `query` finds the conversation with `peer` by its name: the name
/// the screen shows, or the address in hex for a reader who has it from the
/// contact info (1452: the short hash is no part of the name any more).
fn names_match(names: &Names, peer: &[u8; 16], query: &str) -> bool {
    names
        .shown(peer)
        .is_some_and(|name| find(name, query).is_some())
        || find(&crate::address::to_hex(peer), query).is_some()
}
/// recent first. Nothing for an empty query.
pub fn hits(
    conversations: &[Conversation],
    names: &Names,
    order: &[usize],
    query: &str,
    stored: &[StoredHit],
) -> Vec<Hit> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Hit> = order
        .iter()
        .filter(|&&c| names_match(names, &conversations[c].peer, query))
        .map(|&c| Hit::Conversation(c))
        .collect();
    // Only the conversations the list shows: a blocked peer's messages are
    // found in the `blocked` view, not outside it.
    let mut messages: Vec<(u64, Hit)> = Vec::new();
    for &c in order {
        let conversation = &conversations[c];
        for (m, message) in conversation.messages.iter().enumerate() {
            if find(&message.body, query).is_some() {
                let hit = Hit::Message {
                    conversation: c,
                    message: m,
                };
                messages.push((message.activity, hit));
            }
        }
    }
    for (index, hit) in stored.iter().enumerate() {
        if order.iter().any(|&c| conversations[c].peer == hit.peer) {
            messages.push((hit.key, Hit::Stored(index)));
        }
    }
    messages.sort_by_key(|&(activity, _)| core::cmp::Reverse(activity));
    out.extend(messages.into_iter().map(|(_, hit)| hit));
    out
}

/// The messages of `conversation` that match, oldest first: the stops of
/// `n` and `N` in the open conversation.
pub fn matching_messages(conversation: &Conversation, query: &str) -> Vec<usize> {
    conversation
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| find(&m.body, query).is_some())
        .map(|(i, _)| i)
        .collect()
}

/// A preview line `room` cells wide: `text`'s lines joined by a space, the
/// first match marked, and cut so the match is in view: where it lies past
/// the room, the start goes and a `…` stands for it.
pub fn preview(text: &str, query: &str, room: usize) -> String {
    let joined: String = text.split('\n').collect::<Vec<_>>().join(" ");
    let marked = mark_first(&joined, query);
    let chars: Vec<char> = marked.chars().collect();
    let close = chars.iter().position(|&c| c == CLOSE);
    let shown = match close {
        // The close mark and the `…` after it if there is more.
        Some(close) if chars.len() > room && close + 2 > room && room > 2 => {
            let tail = usize::from(close + 1 < chars.len());
            let skip = (close + 1 + tail + 1).saturating_sub(room);
            let mut cut = String::from('…');
            cut.extend(&chars[skip.min(chars.len())..]);
            cut
        }
        _ => marked,
    };
    truncate_to_cols(&shown, room)
}

/// The results laid out for the list pane under the field: every row, and
/// each result's first row among them.
pub struct Laid {
    pub rows: Vec<RLine>,
    /// Beside [`hits`]: where each result's two rows begin.
    pub starts: Vec<usize>,
}

/// `query`'s results `width` cells wide, `selected` behind the bar and,
/// with `focused`, in the selection's colours.
pub fn rows<B: leviculum_ui::input::InputBuffer>(
    model: &UiModel<B>,
    query: &str,
    width: usize,
) -> Laid {
    let found = model.hits();
    let mut laid = Laid {
        rows: Vec::new(),
        starts: Vec::new(),
    };
    if found.is_empty() {
        let mut line = RLine::default();
        push(&mut line, &format!(" {NO_MATCHES}"), RStyle::default());
        laid.rows.push(line);
        return laid;
    }
    let selected = model.search.as_ref().map_or(0, |s| s.selected);
    let focused = model.focus == super::Pane::Conversations;
    let count = |conversations: bool| {
        found
            .iter()
            .filter(|h| matches!(h, Hit::Conversation(_)) == conversations)
            .count()
    };
    for (index, hit) in found.iter().enumerate() {
        let first_of_group = match (index, hit) {
            (0, _) => true,
            (_, Hit::Message { .. } | Hit::Stored(_)) => {
                matches!(found[index - 1], Hit::Conversation(_))
            }
            _ => false,
        };
        if first_of_group {
            let (title, n) = match hit {
                Hit::Conversation(_) => (CONVERSATIONS, count(true)),
                Hit::Message { .. } | Hit::Stored(_) => (MESSAGES, count(false)),
            };
            laid.rows.push(header(title, n, width));
        }
        laid.starts.push(laid.rows.len());
        let [top, bottom] = entry(model, *hit, query, width.saturating_sub(1), model.now);
        for mut row in [top, bottom] {
            if index == selected {
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
            let mut line = RLine::default();
            push(&mut line, " ", RStyle::default());
            line.cells.extend(row.cells);
            laid.rows.push(line);
        }
        laid.rows.push(RLine::default());
    }
    laid
}

/// ` CONVERSATIONS        2 `: the group's name and its count, flush right
/// before the last cell.
fn header(title: &str, count: usize, width: usize) -> RLine {
    let count = format!("{count}");
    let gap = width.saturating_sub(title.chars().count() + count.chars().count() + 2);
    let mut line = RLine::default();
    push(&mut line, " ", RStyle::default());
    push(&mut line, title, bold());
    push(&mut line, &" ".repeat(gap), RStyle::default());
    push(&mut line, &count, RStyle::default());
    push(&mut line, " ", RStyle::default());
    line
}

/// One result's two rows, `width` cells with the bar's margin first and one
/// blank last: who and when, then the preview. A conversation's name carries
/// the mark and its last line is the preview; a message is said by who wrote
/// it, `you to` for ours, over its text with the mark.
fn entry<B>(
    model: &UiModel<B>,
    hit: Hit,
    query: &str,
    width: usize,
    now: Option<WallTime>,
) -> [RLine; 2] {
    let room = width.saturating_sub(2);
    let (conversation, message) = match hit {
        Hit::Conversation(c) => (c, None),
        Hit::Message {
            conversation,
            message,
        } => (conversation, Some(message)),
        Hit::Stored(index) => {
            let found = model.search.as_ref().and_then(|s| s.stored.get(index));
            return match found {
                Some(stored) => stored_entry(model, stored, query, room, now),
                None => [RLine::default(), RLine::default()],
            };
        }
    };
    let conversation = &model.conversations[conversation];
    let shown = message
        .and_then(|m| conversation.messages.get(m))
        .or(conversation.messages.last());
    let time = match (shown.and_then(|m| m.at), now) {
        (Some(at), Some(now)) => super::scene::list_time(&at, &now).resolve(model.lang),
        _ => String::new(),
    };
    let label = model.names.label(&conversation.peer, model.lang);
    let who = match (message, shown) {
        (None, _) => mark_first(&label, query),
        (Some(_), Some(m)) if m.outbound => format!("you to {label}"),
        (Some(_), _) => label,
    };
    let title = truncate_to_cols(&who, room.saturating_sub(time.chars().count() + 1));
    let gap = room.saturating_sub(fit_width(&title) + time.chars().count());
    let mut top = RLine::default();
    push(
        &mut top,
        &format!(" {title}{}{time} ", " ".repeat(gap)),
        RStyle::default(),
    );
    let text = match (message, shown) {
        (Some(_), Some(m)) => preview(&m.body, query, room),
        (None, Some(m)) => {
            let first = m.body.split('\n').next().unwrap_or("");
            let line = match m.outbound {
                true => format!("you: {first}"),
                false => String::from(first),
            };
            truncate_to_cols(&line, room)
        }
        (_, None) => String::new(),
    };
    let gap = room.saturating_sub(text.chars().count());
    let mut bottom = RLine::default();
    push(
        &mut bottom,
        &format!(" {text}{} ", " ".repeat(gap)),
        RStyle::default(),
    );
    [top, bottom]
}

/// A stored result's two rows, as a loaded message's: who and when over the
/// text with the mark.
fn stored_entry<B>(
    model: &UiModel<B>,
    hit: &StoredHit,
    query: &str,
    room: usize,
    now: Option<WallTime>,
) -> [RLine; 2] {
    let time = match (hit.at, now) {
        (Some(at), Some(now)) => super::scene::list_time(&at, &now).resolve(model.lang),
        _ => String::new(),
    };
    let label = model.names.label(&hit.peer, model.lang);
    let who = match hit.outbound {
        true => format!("you to {label}"),
        false => label,
    };
    let title = truncate_to_cols(&who, room.saturating_sub(time.chars().count() + 1));
    let gap = room.saturating_sub(fit_width(&title) + time.chars().count());
    let mut top = RLine::default();
    push(
        &mut top,
        &format!(" {title}{}{time} ", " ".repeat(gap)),
        RStyle::default(),
    );
    let text = preview(&hit.body, query, room);
    let gap = room.saturating_sub(text.chars().count());
    let mut bottom = RLine::default();
    push(
        &mut bottom,
        &format!(" {text}{} ", " ".repeat(gap)),
        RStyle::default(),
    );
    [top, bottom]
}

/// The first result row on screen, of results `laid` in `height` rows: far
/// enough down to show the selected result whole.
pub fn first_row(laid: &Laid, selected: usize, height: usize) -> usize {
    let Some(&start) = laid.starts.get(selected) else {
        return 0;
    };
    (start + 2).saturating_sub(height.max(1))
}
