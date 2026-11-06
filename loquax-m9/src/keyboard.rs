//! The M9's keyboard: the controller's key codes to the key events the key
//! table reads (`handheld-messenger.md`, "The input model"; slice three,
//! 1359, rebuilt on the measured protocol in 1427).
//!
//! What the board has shown (Lew at the device, 2026-11-04 21:03 CET, 13,838
//! reads of register 0x01 of 0x6D, eight bytes each; `thinknode-m9.md`,
//! "The keyboard controller, measured"): the controller does not answer
//! with a matrix. An idle read is all zeros; a key press makes exactly one
//! read non-zero, and that read carries one code in all eight bytes. The
//! codes are ASCII for the characters (`0x61`..`0x7a` a to z, `0x20` the
//! space, `0x30` zero, `0x24` `$`, `0x26` `&`), `0x0d` Enter and `0x08`
//! Backspace, and `0x81`..`0x86`, `0x90` and `0xb4`..`0xb7` for keys that
//! are no character. No read in the capture carries a key-up or a state
//! byte, and the symbols arrive as their own codes: the controller resolves
//! its layers itself and reports presses, nothing else.
//!
//! So the driver keeps no state: one non-zero read is one press, its code
//! is a [`Key`], and a key with a name is the [`KeyEvent`] the shared
//! resolver reads ([`crate::keys::event`]). A key whose code is not named
//! yet is [`Key::Special`], printed `special_0x86`, and makes no event:
//! the reviewer's walk (press each key in layout order, read the `KEY`
//! lines) names it in [`NAMED_SPECIALS`]. Repeat is the controller's: a
//! code read again is a press again; the driver adds none, because no read
//! says a key is still down.

use alloc::string::String;
use core::fmt::Write;

use leviculum_ui::input::KeyEvent;

use crate::keys::{event, M9Key};

/// How often the firmware polls while the panel is lit: below the 15 ms
/// INT pulse the community port measured. At this rate the capture saw
/// every press as one non-zero read, the fastest two 10 ms apart
/// (`0x65` then `0x83` at 9,308,471 and 9,308,481 ms).
pub const POLL_MS: u32 = 10;

/// The codes the walk has named among those that are no character. Empty
/// until the reviewer's walk reads which of `0x81`..`0x86`, `0x90` and
/// `0xb4`..`0xb7` are the arrows, Back and the side buttons: nothing here
/// takes them from a foreign keyboard's table.
pub const NAMED_SPECIALS: &[(u8, M9Key)] = &[];

/// What one code is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A key the M9 screens know.
    Named(M9Key),
    /// A code the controller sends that nothing names yet.
    Special(u8),
}

impl Key {
    /// The key `code` is: printable ASCII a character, `0x0d` Enter,
    /// `0x08` Backspace, a code of [`NAMED_SPECIALS`] its key, any other
    /// [`Key::Special`]. Zero is no key (an idle read).
    pub fn of(code: u8) -> Option<Self> {
        Self::of_in(code, NAMED_SPECIALS)
    }

    fn of_in(code: u8, named: &[(u8, M9Key)]) -> Option<Self> {
        Some(match code {
            0x00 => return None,
            0x08 => Self::Named(M9Key::Backspace),
            0x0d => Self::Named(M9Key::Enter),
            0x20..=0x7e => Self::Named(M9Key::Char(char::from(code))),
            _ => match named.iter().find(|(c, _)| *c == code) {
                Some((_, key)) => Self::Named(*key),
                None => Self::Special(code),
            },
        })
    }

    /// The key event the M9 screens read, none for a key not named yet.
    pub fn event(self) -> Option<KeyEvent> {
        match self {
            Self::Named(key) => Some(event(key)),
            Self::Special(_) => None,
        }
    }

    /// The key's name in a `KEY` line: the character itself, `space`,
    /// `enter`, `backspace`, the navigation keys by name, `special_0x86`.
    pub fn name(self) -> String {
        let mut out = String::new();
        let _ = match self {
            Self::Named(M9Key::Char(' ')) => write!(out, "space"),
            Self::Named(M9Key::Char(c)) => write!(out, "{c}"),
            Self::Named(M9Key::Enter) => write!(out, "enter"),
            Self::Named(M9Key::Backspace) => write!(out, "backspace"),
            Self::Named(M9Key::Back) => write!(out, "back"),
            Self::Named(M9Key::Up) => write!(out, "up"),
            Self::Named(M9Key::Down) => write!(out, "down"),
            Self::Named(M9Key::Left) => write!(out, "left"),
            Self::Named(M9Key::Right) => write!(out, "right"),
            Self::Special(code) => write!(out, "special_{code:#04x}"),
        };
        out
    }
}

/// The line the firmware prints for each press, after its `KBRAW` line:
/// `KEY <ms> code=0x6b name=k`. The walk is read off these lines.
pub fn key_line(ms: u32, code: u8, key: Key) -> String {
    let mut out = String::with_capacity(40);
    let _ = write!(out, "KEY {ms} code={code:#04x} name={}", key.name());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::input::{KeyCode, KeyEventKind};

    #[test]
    fn the_captured_characters_are_their_ascii() {
        for (code, want) in [
            (0x6b, 'k'),
            (0x61, 'a'),
            (0x7a, 'z'),
            (0x20, ' '),
            (0x30, '0'),
            (0x24, '$'),
            (0x26, '&'),
        ] {
            assert_eq!(Key::of(code), Some(Key::Named(M9Key::Char(want))));
        }
        assert_eq!(Key::of(0x0d), Some(Key::Named(M9Key::Enter)));
        assert_eq!(Key::of(0x08), Some(Key::Named(M9Key::Backspace)));
        assert_eq!(Key::of(0x00), None, "an idle read is no key");
    }

    /// The codes the capture saw that are no character stay unnamed until
    /// the walk, and make no event; the positive control names one through
    /// the same lookup and gets its event.
    #[test]
    fn an_unnamed_special_makes_no_event_and_a_named_one_does() {
        for code in [
            0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x90, 0xb4, 0xb5, 0xb6, 0xb7,
        ] {
            let key = Key::of(code).expect("a non-zero code is a key");
            assert_eq!(key, Key::Special(code));
            assert_eq!(key.event(), None);
        }
        assert_eq!(
            Key::of(0x86).map(Key::name).as_deref(),
            Some("special_0x86")
        );
        let named = Key::of_in(0xb5, &[(0xb5, M9Key::Up)]).expect("a key");
        assert_eq!(named, Key::Named(M9Key::Up));
        let up = named.event().expect("a named key is an event");
        assert_eq!((up.code, up.kind), (KeyCode::Up, KeyEventKind::Press));
    }

    #[test]
    fn a_key_line_names_the_code() {
        let line = |code| key_line(9308251, code, Key::of(code).expect("a key"));
        assert_eq!(line(0x6b), "KEY 9308251 code=0x6b name=k");
        assert_eq!(line(0x20), "KEY 9308251 code=0x20 name=space");
        assert_eq!(line(0x0d), "KEY 9308251 code=0x0d name=enter");
        assert_eq!(line(0x08), "KEY 9308251 code=0x08 name=backspace");
        assert_eq!(line(0xb7), "KEY 9308251 code=0xb7 name=special_0xb7");
    }
}
