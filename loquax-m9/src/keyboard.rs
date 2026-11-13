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
//! resolver reads ([`crate::keys::event`]). Repeat is the controller's: a
//! code read again is a press again; the driver adds none, because no read
//! says a key is still down.
//!
//! The keys that are no character (1440) are the M9's own controls above
//! the QWERTY block, left to right (Lew's photo of 2026-11-06): a key with
//! two speech bubbles; a house above a square speech bubble; the ring with
//! four directions and a centre press; a back arrow above a position pin;
//! a map key. Each has a [`Role`], the one a WhatsApp and Google Maps user
//! expects ([`SPECIALS`]). Which code is which key is measured (1459,
//! the reviewer's walk of 2026-11-06 20:17, pressed in the order
//! bubbles, house, square bubble, back, pin, map, ring left, up, down,
//! right, centre): `0x81` the bubbles, `0x82` the house, `0x83` the square
//! bubble, `0x84` the pin, `0x85` the map, `0x86` the back arrow; the ring
//! `0xb4` left, `0xb5` up, `0xb6` down, `0xb7` right. The walk's note had
//! down and right the other way round (the codes rise in the order the
//! keys were pressed, down before right); Lew at the device on 2026-11-07
//! (1528, board log `2026-11-07f`) pressed down and saw nothing, pressed
//! right and saw the selection move down. The ring's centre
//! sends `0x0d`, ASCII CR, which is Enter already (21:03:13, `KEY
//! code=0x0d name=enter`). `0x90`, seen once on 2026-11-04, is no key the
//! walk pressed and stays unnamed. A code nothing
//! names is [`Key::Special`], printed `special_0x91`, and makes no event.

use alloc::string::String;
use core::fmt::Write;

use alloc::vec::Vec;
use leviculum_ui::commands::Command;
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use loquax::ui::{update, UiEffect, UiEvent, UiModel};

use crate::field::Field;
use crate::keys::{event, M9Key};

/// How often the firmware polls while the panel is lit: below the 15 ms
/// INT pulse the community port measured. At this rate the capture saw
/// every press as one non-zero read, the fastest two 10 ms apart
/// (`0x65` then `0x83` at 9,308,471 and 9,308,481 ms).
pub const POLL_MS: u32 = 10;

/// What an M9 control does (1440), by the key's symbol: the roles a
/// user who knows WhatsApp and Google Maps reads off it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Two speech bubbles: the chats list, from anywhere.
    Chats,
    /// The house: the settings page (`house`, 1532, 1561), the home's
    /// state in its top bar. Its command is the chats list's, which the
    /// page takes before it runs.
    Home,
    /// The square speech bubble: a new message, to the contact under the
    /// list's cursor, in its compose line.
    NewMessage,
    /// The back arrow: back one level.
    Back,
    /// The position pin: share my location. Named only until the M9 draws
    /// the position sheet; it does nothing yet, rather than open a question
    /// the M9 cannot show.
    Pin,
    /// The map key: the map tab.
    Map,
    /// The ring: move the selection, switch or page. Its centre sends
    /// Enter (`0x0d`) and needs no role.
    RingLeft,
    RingUp,
    RingDown,
    RingRight,
}

impl Role {
    /// The name a `KEY` line gives it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Chats => "chats",
            Self::Home => "home",
            Self::NewMessage => "new_message",
            Self::Back => "back",
            Self::Pin => "pin",
            Self::Map => "map",
            Self::RingLeft => "ring_left",
            Self::RingUp => "ring_up",
            Self::RingDown => "ring_down",
            Self::RingRight => "ring_right",
        }
    }

    /// The key it is to the shared key table, when it is one: the ring's
    /// arrows and Back.
    pub fn key(self) -> Option<M9Key> {
        Some(match self {
            Self::Back => M9Key::Back,
            Self::RingLeft => M9Key::Left,
            Self::RingUp => M9Key::Up,
            Self::RingDown => M9Key::Down,
            Self::RingRight => M9Key::Right,
            Self::Chats | Self::Home | Self::NewMessage | Self::Pin | Self::Map => return None,
        })
    }

    /// The command it runs, as a GUI's menu runs one, when it is no key:
    /// the terminal's palette verbs for the same roles.
    pub fn command(self) -> Option<Command> {
        match self {
            Self::Chats | Self::Home => Some(Command::FocusConversations),
            Self::NewMessage => Some(Command::FocusCompose),
            Self::Map => Some(Command::Map),
            _ => None,
        }
    }
}

/// The codes that are no character, and their roles, as the reviewer's
/// walk of 2026-11-06 measured them (module docs).
pub const SPECIALS: [(u8, Role); 10] = [
    (0x81, Role::Chats),
    (0x82, Role::Home),
    (0x83, Role::NewMessage),
    (0x84, Role::Pin),
    (0x85, Role::Map),
    (0x86, Role::Back),
    (0xb4, Role::RingLeft),
    (0xb5, Role::RingUp),
    (0xb6, Role::RingDown),
    (0xb7, Role::RingRight),
];

/// What one code is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A key the M9 screens know.
    Named(M9Key),
    /// One of the M9's own controls.
    Role(Role),
    /// A code the controller sends that nothing names yet.
    Special(u8),
}

impl Key {
    /// The key `code` is: printable ASCII a character, `0x0d` Enter,
    /// `0x08` Backspace, a code of [`SPECIALS`] its role, any other
    /// [`Key::Special`]. Zero is no key (an idle read).
    pub fn of(code: u8) -> Option<Self> {
        Self::of_in(code, &SPECIALS)
    }

    fn of_in(code: u8, named: &[(u8, Role)]) -> Option<Self> {
        Some(match code {
            0x00 => return None,
            0x08 => Self::Named(M9Key::Backspace),
            0x0d => Self::Named(M9Key::Enter),
            0x20..=0x7e => Self::Named(M9Key::Char(char::from(code))),
            _ => match named.iter().find(|(c, _)| *c == code) {
                Some((_, role)) => Self::Role(*role),
                None => Self::Special(code),
            },
        })
    }

    /// The key event the M9 screens read: a key's, a role's that is a key
    /// ([`Role::key`]); none for a role that runs a command, nor for a
    /// code not named yet.
    pub fn event(self) -> Option<KeyEvent> {
        match self {
            Self::Named(key) => Some(event(key)),
            Self::Role(role) => role.key().map(event),
            Self::Special(_) => None,
        }
    }

    /// The command a role runs ([`Role::command`]), handed to [`run`].
    pub fn command(self) -> Option<Command> {
        match self {
            Self::Role(role) => role.command(),
            _ => None,
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
            Self::Role(role) => write!(out, "{}", role.name()),
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

/// Run a role's command on `model`, the M9's cursor `row` back at the
/// top: the chats list closes the map first, so it is reached from
/// anywhere.
pub fn run(model: &mut UiModel<Field>, row: &mut usize, command: Command) -> Vec<UiEffect> {
    let mut effects = Vec::new();
    if command == Command::FocusConversations && crate::map::shown(model) {
        let back = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        effects.extend(update(model, UiEvent::Key(back)));
    }
    effects.extend(update(model, UiEvent::Run(command)));
    *row = 0;
    effects
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

    /// The measured table (1459): every control the walk pressed has its
    /// role, in the walk's order; the ring's centre is Enter; a code
    /// nothing names, `0x90` among them, stays a special that makes no
    /// event.
    #[test]
    fn the_specials_have_their_measured_roles() {
        let walk: Vec<(u8, &str)> = SPECIALS
            .iter()
            .map(|(code, role)| (*code, role.name()))
            .collect();
        assert_eq!(
            walk,
            [
                (0x81, "chats"),
                (0x82, "home"),
                (0x83, "new_message"),
                (0x84, "pin"),
                (0x85, "map"),
                (0x86, "back"),
                (0xb4, "ring_left"),
                (0xb5, "ring_up"),
                (0xb6, "ring_down"),
                (0xb7, "ring_right"),
            ]
        );
        let code_of = |key: KeyEvent| key.code;
        let ring = |code: u8| Key::of(code).and_then(Key::event).map(code_of);
        assert_eq!(ring(0xb4), Some(KeyCode::Left));
        assert_eq!(ring(0xb5), Some(KeyCode::Up));
        assert_eq!(ring(0xb6), Some(KeyCode::Down));
        assert_eq!(ring(0xb7), Some(KeyCode::Right));
        assert_eq!(ring(0x0d), Some(KeyCode::Enter), "the centre");
        assert_eq!(ring(0x86), Some(KeyCode::Esc));
        let command = |code: u8| Key::of(code).and_then(Key::command);
        assert_eq!(command(0x81), Some(Command::FocusConversations));
        assert_eq!(command(0x82), Some(Command::FocusConversations));
        assert_eq!(command(0x83), Some(Command::FocusCompose));
        assert_eq!(command(0x85), Some(Command::Map));
        let pin = Key::of(0x84).expect("a key");
        assert_eq!((pin.event(), pin.command()), (None, None));
        for code in [0x90, 0x91, 0x80, 0xff] {
            let key = Key::of(code).expect("a non-zero code is a key");
            assert_eq!(key, Key::Special(code));
            assert_eq!((key.event(), key.command()), (None, None));
        }
        let up = Key::of(0xb5).and_then(Key::event).expect("an event");
        assert_eq!(up.kind, KeyEventKind::Press);
    }

    /// Each role moves the M9's screen as its symbol says, on the fixture
    /// the board opens on: the ring down moves the list's cursor, its
    /// centre opens the chat, the chats key and the house bring the list
    /// back from the chat and from the map, the square bubble opens the
    /// compose line of the contact under the cursor, the map key the map,
    /// the back arrow leaves the chat.
    #[test]
    fn every_role_moves_the_screen_from_the_list() {
        use loquax::ui::Pane;
        let press = |model: &mut UiModel<Field>, row: &mut usize, code: u8| {
            let key = Key::of(code).expect("a key");
            if let Some(event) = key.event() {
                crate::group::press(model, row, &event);
            }
            if let Some(command) = key.command() {
                run(model, row, command);
            }
        };
        let mut row = 0;
        let mut model = crate::fixture::model();
        assert_eq!(model.focus, Pane::Conversations);
        let at = model.selected;
        press(&mut model, &mut row, 0xb6);
        assert_ne!(model.selected, at, "ring down moves the list's cursor");
        press(&mut model, &mut row, 0xb5);
        assert_eq!(model.selected, at, "ring up moves it back");
        press(&mut model, &mut row, 0x0d);
        assert_eq!(model.focus, Pane::Messages, "the centre opens the chat");
        press(&mut model, &mut row, 0x86);
        assert_eq!(model.focus, Pane::Conversations, "back leaves it");
        press(&mut model, &mut row, 0x0d);
        press(&mut model, &mut row, 0x81);
        assert_eq!(model.focus, Pane::Conversations, "the bubbles: the list");
        press(&mut model, &mut row, 0x83);
        assert_eq!(model.focus, Pane::Compose, "the square bubble: compose");
        press(&mut model, &mut row, 0x82);
        assert_eq!(model.focus, Pane::Conversations, "the house: the list");
        press(&mut model, &mut row, 0x85);
        assert!(crate::map::shown(&model), "the map key: the map");
        press(&mut model, &mut row, 0x81);
        assert!(!crate::map::shown(&model), "the bubbles close the map");
        assert_eq!(model.focus, Pane::Conversations);
    }

    #[test]
    fn a_key_line_names_the_code() {
        let line = |code| key_line(9308251, code, Key::of(code).expect("a key"));
        assert_eq!(line(0x6b), "KEY 9308251 code=0x6b name=k");
        assert_eq!(line(0x20), "KEY 9308251 code=0x20 name=space");
        assert_eq!(line(0x0d), "KEY 9308251 code=0x0d name=enter");
        assert_eq!(line(0x08), "KEY 9308251 code=0x08 name=backspace");
        assert_eq!(line(0xb6), "KEY 9308251 code=0xb6 name=ring_down");
        assert_eq!(line(0xb7), "KEY 9308251 code=0xb7 name=ring_right");
        assert_eq!(line(0x90), "KEY 9308251 code=0x90 name=special_0x90");
        assert_eq!(line(0x81), "KEY 9308251 code=0x81 name=chats");
        assert_eq!(line(0x91), "KEY 9308251 code=0x91 name=special_0x91");
    }
}
