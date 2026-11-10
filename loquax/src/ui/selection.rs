//! Text selected over the scene's text runs (1444): what a double click,
//! a triple click, a drag or a Shift+click selects, and the text a copy
//! takes. The core owns the rules and the state's type; a frontend maps a
//! pointer to a [`Spot`] (it has the layout) and keeps one
//! [`TextSelection`] for the open conversation.
//!
//! A run is one text the screen shows (1475): in a conversation a day
//! line, a group's own line ("Anna joined."), a bubble's quote, its words
//! ([`super::scene::BubbleNode::text`]) and its position line, in reading
//! order: the groups top to bottom, each group's bubbles in turn
//! ([`runs`]); in the list each conversation's preview line
//! ([`list_runs`]); in the profile the address and the code
//! ([`profile_runs`]). A [`Spot`] names its run by a [`RunKey`], the
//! message's (or the conversation's) key and the [`Part`] of it, so a
//! selection survives a new message arriving under it; the order between
//! two spots is the runs' order.
//!
//! Offsets are byte offsets into the run's text, always on a character
//! boundary ([`clamp`]).

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use super::scene::{BubbleNode, Chat, Row};
use super::strings::{Lang, StrId};

/// Which text of a message, a conversation or the profile a run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    /// The day line above the group whose first message is the key's.
    Day,
    /// A group's own line ("Anna joined."), the message's.
    Note,
    /// What a reply quotes, inside its bubble.
    Quote,
    /// A bubble's words.
    Body,
    /// A bubble's position line: the coordinates and how alive they are.
    Position,
    /// A conversation's last line in the list, the key the peer's
    /// ([`peer_key`]).
    Preview,
    /// The profile's address, in fours.
    Address,
    /// The profile's code, the text the QR holds.
    Code,
}

impl Part {
    /// A text of a bubble: across bubbles a copy gives one line a bubble,
    /// these joined in it.
    fn in_bubble(self) -> bool {
        matches!(self, Part::Quote | Part::Body | Part::Position)
    }
}

/// Which run: the message's (the conversation's, 0 in the profile) key
/// and the part of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RunKey {
    pub key: u64,
    pub part: Part,
}

impl RunKey {
    pub const fn new(key: u64, part: Part) -> Self {
        Self { key, part }
    }

    /// The words of the bubble `key`.
    pub const fn body(key: u64) -> Self {
        Self::new(key, Part::Body)
    }
}

/// A place in a run: before the character at byte `at` of the run `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Spot {
    pub key: RunKey,
    pub at: usize,
}

/// From where the selection began to where it is now. The two may be in
/// either order; [`TextSelection::ordered`] puts them in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: Spot,
    pub head: Spot,
}

/// One run of selectable text as the screen shows it, and who wrote it
/// and when, for the copy of more than one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRun {
    pub key: RunKey,
    pub text: String,
    /// Empty for a text nobody wrote (a day line, a group's own line, the
    /// profile's): a copy across runs gives it alone on its line.
    pub who: String,
    /// `14:05`, none for a message without a time.
    pub at: Option<String>,
}

impl TextRun {
    fn new(key: RunKey, text: String) -> Self {
        Self {
            key,
            text,
            who: String::new(),
            at: None,
        }
    }
}

/// The open conversation's runs in reading order: each group's day line,
/// then per bubble its quote, its words and its position line, and a
/// group's own lines ("Anna joined.") where they stand. An invitation's
/// card is no run: its words are the card's.
pub fn runs(chat: &Chat, lang: Lang) -> Vec<TextRun> {
    let mut out = Vec::new();
    for group in &chat.groups {
        let who = group.sender.resolve(lang);
        if let (Some(day), Some(first)) = (&group.day, group.bubbles.first()) {
            out.push(TextRun::new(
                RunKey::new(first.bubble.key, Part::Day),
                day.words.resolve(lang),
            ));
        }
        for node in &group.bubbles {
            let key = node.bubble.key;
            if node.bubble.note {
                out.push(TextRun::new(
                    RunKey::new(key, Part::Note),
                    node.bubble.body.clone(),
                ));
                continue;
            }
            let said = |part: Part, text: String| TextRun {
                key: RunKey::new(key, part),
                text,
                who: who.clone(),
                at: node.bubble.at.map(|at| at.hh_mm()),
            };
            if let Some(quote) = &node.bubble.quote {
                out.push(said(Part::Quote, quote.text.clone()));
            }
            if let (None, Some(text)) = (&node.invite, &node.text) {
                out.push(said(Part::Body, text.clone()));
            }
            if let Some(line) = position_line(node, lang) {
                out.push(said(Part::Position, line));
            }
        }
    }
    out
}

/// A bubble's position line as the window draws it: the scene's one
/// sentence (1449 item 5, walk 1 finding 11), `Live position` or
/// `Position from yesterday 21:46`; the coordinates and the accuracy are
/// the Info's. None without a fix.
pub fn position_line(node: &BubbleNode, lang: Lang) -> Option<String> {
    node.position.as_ref().map(|words| words.resolve(lang))
}

/// A conversation's key among the list's runs: its peer's first eight
/// bytes.
pub fn peer_key(peer: &[u8; 16]) -> u64 {
    let mut first = [0u8; 8];
    first.copy_from_slice(&peer[..8]);
    u64::from_le_bytes(first)
}

/// A list row's last line as the window draws it under the name: the
/// draft that stands in for it, a group's invitation or its count while
/// nothing was written, a report's words after the pin, a text of ours
/// said as ours, else the last line itself.
pub fn preview_line(r: &Row, lang: Lang) -> String {
    let content = &r.row;
    let invited = r
        .group
        .as_ref()
        .filter(|g| content.last.is_empty() || g.standing == super::groups::Standing::Invited);
    match (&r.draft, &r.live, content.ours && content.kind == "text") {
        (Some(draft), _, _) => draft.resolve(lang),
        (None, None, _) if invited.is_some() => {
            invited.map(|g| g.words.resolve(lang)).unwrap_or_default()
        }
        (None, Some(live), _) => format!("{} {}", super::chat::PIN, live.resolve(lang)),
        (None, None, true) => format!("{}: {}", StrId::QuoteYou.text(lang), content.last),
        (None, None, false) => content.last.clone(),
    }
}

/// The list's runs, the rows top to bottom: each one's last line, its
/// writer the conversation's name and its time the row's.
pub fn list_runs<'a>(rows: impl IntoIterator<Item = &'a Row>, lang: Lang) -> Vec<TextRun> {
    rows.into_iter()
        .map(|r| TextRun {
            key: RunKey::new(peer_key(&r.row.peer), Part::Preview),
            text: preview_line(r, lang),
            who: r.row.name.clone(),
            at: r.row.at.map(|at| at.hh_mm()),
        })
        .collect()
}

/// The profile's runs: the address in fours, then the code the QR holds.
pub fn profile_runs(profile: &super::profile::ProfileScreen) -> Vec<TextRun> {
    let mut out = vec![TextRun::new(
        RunKey::new(0, Part::Address),
        profile.fours.clone(),
    )];
    if let Some(code) = &profile.code {
        out.push(TextRun::new(RunKey::new(0, Part::Code), code.clone()));
    }
    out
}

/// What a copy takes of `range` of `run`: the text as it stands, an
/// address without the blanks between its fours, which only help the eye.
fn copied(run: &TextRun, range: Range<usize>) -> String {
    let text = &run.text[range];
    match run.key.part {
        Part::Address => text.chars().filter(|c| *c != ' ').collect(),
        _ => String::from(text),
    }
}

/// `at` moved back onto a character boundary of `text`, and to its end at
/// most.
pub fn clamp(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// What kind of character a double click groups: a word's letters and
/// digits (and `_`, `'` inside a word), blanks, or anything else, which
/// stands alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Blank,
    Other,
}

fn class(c: char) -> Class {
    match c {
        c if c.is_alphanumeric() || c == '_' => Class::Word,
        c if c.is_whitespace() => Class::Blank,
        _ => Class::Other,
    }
}

/// The word at byte `at` of `text`, as a double click selects it: the
/// run of letters and digits the character at `at` belongs to (an
/// apostrophe between two letters joins them: `don't`), the run of
/// blanks, or the one other character. At the end of the text, the
/// character before. Empty only for an empty text. A newline is never
/// part of what is selected.
pub fn word_at(text: &str, at: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let at = clamp(text, at);
    // The character the press is on; past the end, the last one.
    let (start, c) = match text[at..].chars().next() {
        Some(c) if c != '\n' || at == 0 => (at, c),
        _ => match text[..at].char_indices().next_back() {
            Some((i, c)) => (i, c),
            None => return at..at,
        },
    };
    if c == '\n' {
        return start..start;
    }
    let kind = class(c);
    if kind == Class::Other && !is_inner_apostrophe(text, start) {
        return start..start + c.len_utf8();
    }
    let same = |i: usize, c: char| -> bool {
        match kind {
            Class::Word => class(c) == Class::Word || is_inner_apostrophe(text, i),
            Class::Blank => c != '\n' && class(c) == Class::Blank,
            Class::Other => class(c) == Class::Word || is_inner_apostrophe(text, i),
        }
    };
    let mut from = start;
    for (i, c) in text[..start].char_indices().rev() {
        if !same(i, c) {
            break;
        }
        from = i;
    }
    let mut to = start;
    for (i, c) in text[start..].char_indices() {
        if !same(start + i, c) {
            break;
        }
        to = start + i + c.len_utf8();
    }
    from..to
}

/// Whether byte `i` of `text` is an apostrophe with a letter on each side.
fn is_inner_apostrophe(text: &str, i: usize) -> bool {
    let Some(c) = text[i..].chars().next() else {
        return false;
    };
    if c != '\'' && c != '\u{2019}' {
        return false;
    }
    let before = text[..i].chars().next_back();
    let after = text[i + c.len_utf8()..].chars().next();
    matches!((before, after), (Some(b), Some(a)) if class(b) == Class::Word && class(a) == Class::Word)
}

/// The line at byte `at` of `text`, as a triple click selects it: the
/// paragraph between two newlines, without them.
pub fn line_at(text: &str, at: usize) -> Range<usize> {
    let at = clamp(text, at);
    let from = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let to = text[at..].find('\n').map_or(text.len(), |i| at + i);
    from..to
}

impl TextSelection {
    /// Nothing selected yet, the press at `spot`.
    pub fn caret(spot: Spot) -> Self {
        Self {
            anchor: spot,
            head: spot,
        }
    }

    /// `range` of the run `key`, the anchor at its start.
    pub fn within(key: RunKey, range: Range<usize>) -> Self {
        Self {
            anchor: Spot {
                key,
                at: range.start,
            },
            head: Spot { key, at: range.end },
        }
    }

    /// Everything in `runs`, none when there is no run.
    pub fn all(runs: &[TextRun]) -> Option<Self> {
        let (first, last) = (runs.first()?, runs.last()?);
        Some(Self {
            anchor: Spot {
                key: first.key,
                at: 0,
            },
            head: Spot {
                key: last.key,
                at: last.text.len(),
            },
        })
    }

    /// The selection with its head moved to `head`: a drag, a Shift+click.
    pub fn extend(self, head: Spot) -> Self {
        Self { head, ..self }
    }

    /// The two ends in reading order, by the runs' places; none when an end
    /// is in no run (its message is gone).
    pub fn ordered(&self, runs: &[TextRun]) -> Option<(usize, Spot, usize, Spot)> {
        let place = |s: &Spot| runs.iter().position(|r| r.key == s.key);
        let (a, h) = (place(&self.anchor)?, place(&self.head)?);
        let anchor = (a, clamp(&runs[a].text, self.anchor.at));
        let head = (h, clamp(&runs[h].text, self.head.at));
        let ((i, x), (j, y)) = match anchor <= head {
            true => (anchor, head),
            false => (head, anchor),
        };
        let spot = |n: usize, at: usize| Spot {
            key: runs[n].key,
            at,
        };
        Some((i, spot(i, x), j, spot(j, y)))
    }

    /// Whether it selects nothing.
    pub fn is_empty(&self, runs: &[TextRun]) -> bool {
        match self.ordered(runs) {
            Some((i, a, j, b)) => i == j && a.at == b.at,
            None => true,
        }
    }

    /// The selected bytes of the run `key`, none where it selects nothing
    /// of it.
    pub fn range_in(&self, runs: &[TextRun], key: RunKey) -> Option<Range<usize>> {
        let (i, a, j, b) = self.ordered(runs)?;
        let n = runs.iter().position(|r| r.key == key)?;
        if n < i || n > j {
            return None;
        }
        let from = if n == i { a.at } else { 0 };
        let to = if n == j { b.at } else { runs[n].text.len() };
        (from < to).then_some(from..to)
    }

    /// The text a copy takes: within one run its words as they are;
    /// across runs one line a bubble, `Name, 14:05: text`, a bubble's
    /// quote (`> `), words and position line joined in it by ` · `; a
    /// text nobody wrote (a day line, a group's own line, the profile's)
    /// alone on its line.
    pub fn text(&self, runs: &[TextRun]) -> String {
        let Some((i, a, j, b)) = self.ordered(runs) else {
            return String::new();
        };
        if i == j {
            return copied(&runs[i], a.at..b.at);
        }
        let mut out = String::new();
        // The bubble the line being written is, to join its parts.
        let mut open: Option<u64> = None;
        for run in runs.iter().take(j + 1).skip(i) {
            let Some(range) = self.range_in(runs, run.key) else {
                continue;
            };
            let words = copied(run, range);
            let quote = match run.key.part {
                Part::Quote => "> ",
                _ => "",
            };
            let bubble = run.key.part.in_bubble().then_some(run.key.key);
            if bubble.is_some() && bubble == open {
                out.push_str(" · ");
                out.push_str(quote);
                out.push_str(&words);
                continue;
            }
            open = bubble;
            if !out.is_empty() {
                out.push('\n');
            }
            match (&run.at, run.who.is_empty()) {
                (_, true) => {}
                (Some(at), false) => out.push_str(&format!("{}, {at}: ", run.who)),
                (None, false) => out.push_str(&format!("{}: ", run.who)),
            }
            out.push_str(quote);
            out.push_str(&words);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn word(text: &str, at: usize) -> &str {
        &text[word_at(text, at)]
    }

    #[test]
    fn a_double_click_selects_the_word_it_is_on() {
        let text = "Bring the apples, please!";
        assert_eq!(word(text, 0), "Bring");
        assert_eq!(word(text, 3), "Bring");
        assert_eq!(word(text, 12), "apples");
        // The comma and the blank stand for themselves.
        assert_eq!(word(text, 16), ",");
        assert_eq!(word(text, 17), " ");
        // Past the end, the last character's word: the bang.
        assert_eq!(word(text, 99), "!");
        assert_eq!(word("", 0), "");
    }

    #[test]
    fn a_word_keeps_its_umlauts_digits_and_inner_apostrophe() {
        let text = "Grüße um 14:05, don't 'quote'";
        assert_eq!(word(text, 2), "Grüße");
        assert_eq!(word(text, 11), "14");
        assert_eq!(word(text, 13), ":");
        assert_eq!(word(text, 18), "don't");
        assert_eq!(word(text, 21), "don't", "on the apostrophe itself");
        // An apostrophe at a word's edge is no part of it.
        assert_eq!(word(text, 24), "'");
        assert_eq!(word(text, 25), "quote");
        // A byte inside a character goes back to its start.
        assert_eq!(word(text, 3), "Grüße");
    }

    #[test]
    fn a_double_click_never_selects_a_newline() {
        let text = "one\ntwo";
        assert_eq!(word(text, 3), "one", "the end of a line is its last word");
        assert_eq!(word(text, 4), "two");
        assert_eq!(word("a\n\nb", 2), "", "an empty line selects nothing");
    }

    #[test]
    fn a_triple_click_selects_the_paragraph() {
        let text = "first line\nsecond one\nthird";
        assert_eq!(&text[line_at(text, 3)], "first line");
        assert_eq!(&text[line_at(text, 11)], "second one");
        assert_eq!(&text[line_at(text, 99)], "third");
        let alone = "alone";
        assert_eq!(&alone[line_at(alone, 2)], "alone");
    }

    fn runs3() -> Vec<TextRun> {
        vec![
            TextRun {
                key: RunKey::body(10),
                text: "Hello there".to_string(),
                who: "Anna".to_string(),
                at: Some("14:05".to_string()),
            },
            TextRun {
                key: RunKey::body(11),
                text: "How are you?".to_string(),
                who: "Anna".to_string(),
                at: Some("14:06".to_string()),
            },
            TextRun {
                key: RunKey::body(7),
                text: "Fine".to_string(),
                who: "You".to_string(),
                at: None,
            },
        ]
    }

    #[test]
    fn within_one_bubble_the_copy_is_the_words_alone() {
        let runs = runs3();
        let s = TextSelection::within(RunKey::body(10), 6..11);
        assert_eq!(s.text(&runs), "there");
        assert_eq!(s.range_in(&runs, RunKey::body(10)), Some(6..11));
        assert_eq!(s.range_in(&runs, RunKey::body(11)), None);
        // Backwards is the same selection.
        let back = TextSelection {
            anchor: Spot {
                key: RunKey::body(10),
                at: 11,
            },
            head: Spot {
                key: RunKey::body(10),
                at: 6,
            },
        };
        assert_eq!(back.text(&runs), "there");
    }

    #[test]
    fn across_bubbles_each_line_names_the_writer_and_the_time() {
        let runs = runs3();
        // From the middle of the first to the middle of the third, the
        // head before the anchor in reading order.
        let s = TextSelection {
            anchor: Spot {
                key: RunKey::body(7),
                at: 2,
            },
            head: Spot {
                key: RunKey::body(10),
                at: 6,
            },
        };
        assert_eq!(
            s.text(&runs),
            "Anna, 14:05: there\nAnna, 14:06: How are you?\nYou: Fi"
        );
        assert_eq!(s.range_in(&runs, RunKey::body(11)), Some(0..12));
        assert_eq!(s.range_in(&runs, RunKey::body(7)), Some(0..2));
    }

    #[test]
    fn a_caret_is_empty_and_extending_it_selects() {
        let runs = runs3();
        let caret = TextSelection::caret(Spot {
            key: RunKey::body(11),
            at: 4,
        });
        assert!(caret.is_empty(&runs));
        assert_eq!(caret.text(&runs), "");
        let s = caret.extend(Spot {
            key: RunKey::body(11),
            at: 7,
        });
        assert!(!s.is_empty(&runs));
        assert_eq!(s.text(&runs), "are");
        // A spot whose message is gone selects nothing.
        let gone = TextSelection::caret(Spot {
            key: RunKey::body(99),
            at: 0,
        })
        .extend(Spot {
            key: RunKey::body(10),
            at: 3,
        });
        assert!(gone.is_empty(&runs));
        assert_eq!(gone.text(&runs), "");
    }

    #[test]
    fn select_all_spans_every_run() {
        let runs = runs3();
        let all = TextSelection::all(&runs).expect("runs");
        assert_eq!(
            all.text(&runs),
            "Anna, 14:05: Hello there\nAnna, 14:06: How are you?\nYou: Fine"
        );
        assert_eq!(TextSelection::all(&[]), None);
    }

    fn run(key: u64, part: Part, text: &str, who: &str, at: Option<&str>) -> TextRun {
        TextRun {
            key: RunKey::new(key, part),
            text: text.to_string(),
            who: who.to_string(),
            at: at.map(|a| a.to_string()),
        }
    }

    /// A day line, a reply with its quote and its position, a group's own
    /// line and a bubble after it (1475).
    fn mixed() -> Vec<TextRun> {
        vec![
            run(10, Part::Day, "Today", "", None),
            run(10, Part::Quote, "Where are you?", "Anna", Some("14:05")),
            run(10, Part::Body, "Here", "Anna", Some("14:05")),
            run(
                10,
                Part::Position,
                "52.5200, 13.4050",
                "Anna",
                Some("14:05"),
            ),
            run(11, Part::Note, "Ben joined.", "", None),
            run(12, Part::Body, "Hi all", "Ben", None),
        ]
    }

    #[test]
    fn a_bubbles_quote_words_and_position_copy_as_one_line() {
        let runs = mixed();
        let all = TextSelection::all(&runs).expect("runs");
        assert_eq!(
            all.text(&runs),
            "Today\nAnna, 14:05: > Where are you? · Here · 52.5200, 13.4050\nBen joined.\nBen: Hi all"
        );
        // From the middle of the words to the middle of the position: the
        // bubble's line begins at the words.
        let part = TextSelection {
            anchor: Spot {
                key: RunKey::new(10, Part::Body),
                at: 1,
            },
            head: Spot {
                key: RunKey::new(10, Part::Position),
                at: 7,
            },
        };
        assert_eq!(part.text(&runs), "Anna, 14:05: ere · 52.5200");
    }

    #[test]
    fn a_text_nobody_wrote_selects_and_copies_alone() {
        let runs = mixed();
        let day = TextSelection::within(RunKey::new(10, Part::Day), word_at("Today", 2));
        assert_eq!(day.text(&runs), "Today");
        let note = TextSelection::within(RunKey::new(11, Part::Note), 0..3);
        assert_eq!(note.text(&runs), "Ben");
        // A quote alone is its words, no mark.
        let quote = TextSelection::within(RunKey::new(10, Part::Quote), 0..5);
        assert_eq!(quote.text(&runs), "Where");
        // The parts of one message are different runs: the quote's range
        // is not the words'.
        assert_eq!(quote.range_in(&runs, RunKey::new(10, Part::Body)), None);
        assert_eq!(
            quote.range_in(&runs, RunKey::new(10, Part::Quote)),
            Some(0..5)
        );
    }

    #[test]
    fn the_list_previews_copy_one_line_a_conversation() {
        let runs = vec![
            run(1, Part::Preview, "See you", "Anna", Some("09:12")),
            run(2, Part::Preview, "You: ok", "Ben", None),
        ];
        let all = TextSelection::all(&runs).expect("runs");
        assert_eq!(all.text(&runs), "Anna, 09:12: See you\nBen: You: ok");
        assert_eq!(
            peer_key(&[1, 0, 0, 0, 0, 0, 0, 0, 9, 9, 9, 9, 9, 9, 9, 9]),
            1
        );
    }

    #[test]
    fn the_address_copies_without_the_blanks_between_its_fours() {
        let runs = vec![
            run(0, Part::Address, "a1b2 c3d4 e5f6", "", None),
            run(0, Part::Code, "lxma://a1b2c3d4e5f6", "", None),
        ];
        let address = TextSelection::within(RunKey::new(0, Part::Address), 0..14);
        assert_eq!(address.text(&runs), "a1b2c3d4e5f6");
        let both = TextSelection::all(&runs).expect("runs");
        assert_eq!(both.text(&runs), "a1b2c3d4e5f6\nlxma://a1b2c3d4e5f6");
        // A double click on the fours takes one four, as the eye groups it.
        assert_eq!(&runs[0].text[word_at(&runs[0].text, 6)], "c3d4");
    }
}
