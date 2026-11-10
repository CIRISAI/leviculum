//! Pairing the M9 with a home by the code typed on its keys (1359's
//! decision, 1363): the board has no camera and no clipboard, so the 140
//! characters of the admission code the phone's "Link a device" screen
//! shows are typed here, checked as they come
//! ([`loquax::home::code::typing`]), and on Enter the complete code becomes
//! the join the board's executor performs ([`loquax::home::ask::Ask::join`]).
//!
//! What the screen shows is [`Pairing::lines`]: the code typed so far in
//! groups of five, wrapped to the panel's 32 cells less the margin, and one line saying
//! how far it came or which character cannot stand where it stands.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::input::{InputBuffer, KeyCode, KeyEvent, KeyEventKind};
use loquax::home::ask::{Ask, HomeError};
use loquax::home::code::{self, HomeCode, Typing, TYPED_LEN};

use crate::field::Field;

/// The name the board gives itself at its home: what the phone's device
/// list shows.
pub const DEVICE_NAME: &str = "M9";

/// Cells a code line may take: the panel's, less the margin cell the
/// screen puts before it. Five groups of five: 29.
const WIDTH: usize = crate::screen::COLS - 1;

/// The code being typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pairing {
    field: Field,
    /// An error that is a state (1493): the code checked but flash would
    /// not keep it. Shown in the verdict's place until the next key.
    pub note: Option<&'static str>,
}

impl Pairing {
    /// Nothing typed yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// One key. Characters, Backspace and the cursor keys edit the code;
    /// Enter on a complete code with a good checksum is the code to join
    /// with, and on anything else changes nothing.
    pub fn key(&mut self, key: &KeyEvent) -> Option<HomeCode> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        self.note = None;
        if key.code == KeyCode::Enter {
            return match self.typing() {
                Typing::Complete(Ok(code)) => Some(code),
                _ => None,
            };
        }
        self.field.edit(key);
        None
    }

    /// Where the code typed so far stands.
    pub fn typing(&self) -> Typing {
        code::typing(self.field.value())
    }

    /// The join of the complete code, begun at `now_ms` with `budget_ms`.
    pub fn join(code: &HomeCode, now_ms: u64, budget_ms: u64) -> Result<Ask, HomeError> {
        Ask::join(code, DEVICE_NAME, now_ms, budget_ms)
    }

    /// The screen: the code in fives, wrapped at a group's end, then the
    /// verdict.
    pub fn lines(&self) -> Vec<String> {
        let grouped = code::grouped(self.field.value());
        let mut lines = Vec::new();
        let mut line = String::new();
        for group in grouped.split(' ').filter(|group| !group.is_empty()) {
            if !line.is_empty() && line.len() + 1 + group.len() > WIDTH {
                lines.push(core::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(group);
        }
        lines.push(line);
        lines.push(match self.note {
            Some(note) => String::from(note),
            None => verdict(&self.typing()),
        });
        lines
    }
}

/// The pairing screen, `cols` x `rows` cells (1415): a bar saying what
/// it is for and, once a code was taken, how the join goes (`home`), the
/// code typed so far in fives under it, the verdict, and the two keys
/// that matter. The words are the code's own (`verdict`), which the
/// pairing has said in English since 1363.
pub fn screen(
    pairing: &Pairing,
    home: Option<loquax::home::board::Word>,
    look: &crate::screen::Look,
    cols: usize,
    rows: usize,
) -> Vec<leviculum_ui::style::RLine> {
    use crate::screen::{fill, painted, push};
    use leviculum_ui::style::{RLine, RStyle};
    let chrome = painted(look.tokens.chrome);
    let plain = RStyle::default();
    let text = |words: &str, style| {
        let mut line = RLine::default();
        push(&mut line, words, style);
        fill(line, cols, style)
    };
    let title = match home {
        Some(word) => format!(" {}", word.text().resolve(look.lang)),
        None => String::from(" Link this M9 to your mailbox"),
    };
    let mut out = alloc::vec![text(&title, chrome), text("", plain)];
    out.push(text(" Type your mailbox's code:", plain));
    let mut lines = pairing.lines();
    let verdict = lines.pop().unwrap_or_default();
    for line in lines {
        out.push(text(&format!(" {line}"), plain));
    }
    out.push(text("", plain));
    out.push(text(&format!(" {verdict}"), plain));
    while out.len() + 1 < rows {
        out.push(text("", plain));
    }
    out.push(text(" Enter links   Back leaves", chrome));
    out.truncate(rows);
    out
}

/// The `FRAME_DUMP` screen byte of the pairing screen (1440): the byte
/// after the fixture's screens ([`crate::fixture::SCREENS`]). The pairing
/// is no screen of the model but a text drawn over it, so it is not one
/// of them: the census and the walks have nothing to measure in it. The
/// board dumps it fresh, nothing typed, no home: the screen a board
/// without a client record opens on, golden `m9-fw-pairing.png`.
pub const DUMP_BYTE: u8 = crate::fixture::SCREENS.len() as u8;

/// Its name in the board's `[DUMP]` line and the golden's file name.
pub const DUMP_NAME: &str = "pairing";

/// Draw `pairing` as the board draws it, over `look`, with the home's
/// word in the title once a code was taken.
pub fn draw(
    renderer: &mut crate::render::Renderer<'_, crate::frame::Frame>,
    pairing: &Pairing,
    home: Option<loquax::home::board::Word>,
    look: &crate::screen::Look,
) -> Result<(), crate::render::DrawError> {
    renderer.text(|cols, rows| screen(pairing, home, look, cols, rows))
}

/// One line on how far the code came.
fn verdict(typing: &Typing) -> String {
    match typing {
        Typing::Short { typed } => format!("{typed} of {TYPED_LEN} typed"),
        Typing::Stray { at } => format!("character {} is out of place", at + 1),
        Typing::Complete(Ok(_)) => String::from("complete: Enter links"),
        Typing::Complete(Err(error)) => format!("{error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{event, M9Key};

    fn code() -> HomeCode {
        HomeCode {
            public_key: core::array::from_fn(|i| i as u8),
            token: [7; 16],
        }
    }

    fn typed(pairing: &mut Pairing, text: &str) -> Option<HomeCode> {
        text.chars()
            .filter_map(|c| pairing.key(&event(M9Key::Char(c))))
            .last()
    }

    /// A stray character is named where it stands, Backspace takes it
    /// back, and Enter joins only once the code is whole and checks.
    #[test]
    fn a_stray_is_named_at_once_and_enter_waits_for_a_good_code() {
        let code = code();
        let text = code.encode();
        let mut pairing = Pairing::new();
        typed(&mut pairing, &text[..12]);
        assert_eq!(pairing.typing(), Typing::Short { typed: 12 });
        pairing.key(&event(M9Key::Char('9')));
        assert_eq!(pairing.typing(), Typing::Stray { at: 12 });
        assert_eq!(pairing.lines()[1], "character 13 is out of place");
        assert_eq!(pairing.key(&event(M9Key::Enter)), None);
        pairing.key(&event(M9Key::Backspace));
        typed(&mut pairing, &text[12..]);
        assert_eq!(pairing.key(&event(M9Key::Enter)), Some(code.clone()));
        let lines = pairing.lines();
        assert!(lines.iter().all(|line| line.chars().count() <= WIDTH));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("complete: Enter links")
        );
        // Control: one character changed is the checksum's typo, no join.
        let mut wrong = text.clone();
        let other = if &wrong[40..41] == "a" { "b" } else { "a" };
        wrong.replace_range(40..41, other);
        let mut pairing = Pairing::new();
        typed(&mut pairing, &wrong);
        assert!(matches!(pairing.typing(), Typing::Complete(Err(_))));
        assert_eq!(pairing.key(&event(M9Key::Enter)), None);
    }
}
