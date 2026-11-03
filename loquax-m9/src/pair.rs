//! Pairing the M9 with a home by the code typed on its keys (1359's
//! decision, 1363): the board has no camera and no clipboard, so the 140
//! characters of the admission code the phone's "Link a device" screen
//! shows are typed here, checked as they come
//! ([`loquax::home::code::typing`]), and on Enter the complete code becomes
//! the join the board's executor performs ([`loquax::home::ask::Ask::join`]).
//!
//! What the screen shows is [`Pairing::lines`]: the code typed so far in
//! groups of five, wrapped to the panel's 53 cells, and one line saying
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

/// Cells on one line of the panel.
const WIDTH: usize = 53;

/// The code being typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pairing {
    field: Field,
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
        lines.push(verdict(&self.typing()));
        lines
    }
}

/// One line on how far the code came.
fn verdict(typing: &Typing) -> String {
    match typing {
        Typing::Short { typed } => format!("{typed} of {TYPED_LEN} typed"),
        Typing::Stray { at } => format!("character {} cannot stand there", at + 1),
        Typing::Complete(Ok(_)) => String::from("complete: Enter links this device"),
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
        assert_eq!(pairing.lines()[1], "character 13 cannot stand there");
        assert_eq!(pairing.key(&event(M9Key::Enter)), None);
        pairing.key(&event(M9Key::Backspace));
        typed(&mut pairing, &text[12..]);
        assert_eq!(pairing.key(&event(M9Key::Enter)), Some(code.clone()));
        let lines = pairing.lines();
        assert!(lines.iter().all(|line| line.chars().count() <= WIDTH));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("complete: Enter links this device")
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
