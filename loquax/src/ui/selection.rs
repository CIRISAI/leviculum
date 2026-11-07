//! Text selected over the scene's text runs (1444): what a double click,
//! a triple click, a drag or a Shift+click selects, and the text a copy
//! takes. The core owns the rules and the state's type; a frontend maps a
//! pointer to a [`Spot`] (it has the layout) and keeps one
//! [`TextSelection`] for the open conversation.
//!
//! A run is a bubble's words ([`super::scene::BubbleNode::text`]), in
//! reading order: the conversation's groups top to bottom, each group's
//! bubbles in turn. A [`Spot`] names its run by the message's key, so a
//! selection survives a new message arriving under it; the order between
//! two spots is the runs' order ([`runs`]).
//!
//! Offsets are byte offsets into the run's text, always on a character
//! boundary ([`clamp`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use super::scene::Chat;
use super::strings::Lang;

/// A place in a run: before the character at byte `at` of the run of the
/// message `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Spot {
    pub key: u64,
    pub at: usize,
}

/// From where the selection began to where it is now. The two may be in
/// either order; [`TextSelection::ordered`] puts them in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: Spot,
    pub head: Spot,
}

/// One run of selectable text: a bubble's words, who wrote them and when,
/// for the copy of more than one bubble.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRun {
    pub key: u64,
    pub text: String,
    pub who: String,
    /// `14:05`, none for a message without a time.
    pub at: Option<String>,
}

/// The open conversation's runs in reading order: every bubble with words,
/// a group's own lines ("Anna joined.") not among them.
pub fn runs(chat: &Chat, lang: Lang) -> Vec<TextRun> {
    let mut out = Vec::new();
    for group in &chat.groups {
        let who = group.sender.resolve(lang);
        for node in &group.bubbles {
            if node.bubble.note {
                continue;
            }
            let Some(text) = &node.text else {
                continue;
            };
            out.push(TextRun {
                key: node.bubble.key,
                text: text.clone(),
                who: who.clone(),
                at: node.bubble.at.map(|at| at.hh_mm()),
            });
        }
    }
    out
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
    pub fn within(key: u64, range: Range<usize>) -> Self {
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
    pub fn range_in(&self, runs: &[TextRun], key: u64) -> Option<Range<usize>> {
        let (i, a, j, b) = self.ordered(runs)?;
        let n = runs.iter().position(|r| r.key == key)?;
        if n < i || n > j {
            return None;
        }
        let from = if n == i { a.at } else { 0 };
        let to = if n == j { b.at } else { runs[n].text.len() };
        (from < to).then_some(from..to)
    }

    /// The text a copy takes: within one bubble its words as they are;
    /// across bubbles one line per bubble, `Name, 14:05: text`.
    pub fn text(&self, runs: &[TextRun]) -> String {
        let Some((i, a, j, b)) = self.ordered(runs) else {
            return String::new();
        };
        if i == j {
            return String::from(&runs[i].text[a.at..b.at]);
        }
        let mut out = String::new();
        for run in runs.iter().take(j + 1).skip(i) {
            let Some(range) = self.range_in(runs, run.key) else {
                continue;
            };
            if !out.is_empty() {
                out.push('\n');
            }
            let head = match &run.at {
                Some(at) => format!("{}, {at}: ", run.who),
                None => format!("{}: ", run.who),
            };
            out.push_str(&head);
            out.push_str(&run.text[range]);
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
                key: 10,
                text: "Hello there".to_string(),
                who: "Anna".to_string(),
                at: Some("14:05".to_string()),
            },
            TextRun {
                key: 11,
                text: "How are you?".to_string(),
                who: "Anna".to_string(),
                at: Some("14:06".to_string()),
            },
            TextRun {
                key: 7,
                text: "Fine".to_string(),
                who: "You".to_string(),
                at: None,
            },
        ]
    }

    #[test]
    fn within_one_bubble_the_copy_is_the_words_alone() {
        let runs = runs3();
        let s = TextSelection::within(10, 6..11);
        assert_eq!(s.text(&runs), "there");
        assert_eq!(s.range_in(&runs, 10), Some(6..11));
        assert_eq!(s.range_in(&runs, 11), None);
        // Backwards is the same selection.
        let back = TextSelection {
            anchor: Spot { key: 10, at: 11 },
            head: Spot { key: 10, at: 6 },
        };
        assert_eq!(back.text(&runs), "there");
    }

    #[test]
    fn across_bubbles_each_line_names_the_writer_and_the_time() {
        let runs = runs3();
        // From the middle of the first to the middle of the third, the
        // head before the anchor in reading order.
        let s = TextSelection {
            anchor: Spot { key: 7, at: 2 },
            head: Spot { key: 10, at: 6 },
        };
        assert_eq!(
            s.text(&runs),
            "Anna, 14:05: there\nAnna, 14:06: How are you?\nYou: Fi"
        );
        assert_eq!(s.range_in(&runs, 11), Some(0..12));
        assert_eq!(s.range_in(&runs, 7), Some(0..2));
    }

    #[test]
    fn a_caret_is_empty_and_extending_it_selects() {
        let runs = runs3();
        let caret = TextSelection::caret(Spot { key: 11, at: 4 });
        assert!(caret.is_empty(&runs));
        assert_eq!(caret.text(&runs), "");
        let s = caret.extend(Spot { key: 11, at: 7 });
        assert!(!s.is_empty(&runs));
        assert_eq!(s.text(&runs), "are");
        // A spot whose message is gone selects nothing.
        let gone = TextSelection::caret(Spot { key: 99, at: 0 }).extend(Spot { key: 10, at: 3 });
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
}
