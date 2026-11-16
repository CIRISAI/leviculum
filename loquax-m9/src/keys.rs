//! The M9's keys as the key events the terminal's resolver reads
//! (`handheld-messenger.md`, "The input model"): a key becomes a
//! `KeyEvent`, the shared table resolves it (`loquax::keys::resolve`) and
//! the action becomes an intent (`Intent::from_action`). Nothing here is
//! M9-specific past the first step.
//!
//! The 37-key QWERTY reaches the board through its I2C controller, with
//! navigation keys beside the letters (`thinknode-m9.md`, "Keyboard").
//! Which scancode is which key, the layers and the repeat are read by the
//! keyboard slice on the device; this table starts where a scancode has
//! become a named key, and holds the list's rows of slice one and the
//! chat's and the compose line's of slice two. A letter is the key the
//! terminal binds to the same verb (`i` writes), so the M9 binds nothing
//! of its own; what no row binds in the compose line is the field's edit
//! ([`crate::field::Field`]). The group's verbs have no chord in the
//! terminal, only its palette, so their keys run the scene node's action
//! instead and live in [`crate::group`] (slice six, 1366).

use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use loquax::ui::Pane;

/// A key of the M9's keyboard, by what it is labelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M9Key {
    /// The navigation keys.
    Up,
    Down,
    Left,
    Right,
    /// Enter, beside the letters.
    Enter,
    /// The navigation cluster's back key.
    Back,
    /// The key that erases the character before the cursor.
    Backspace,
    /// A letter, a digit, the space, or what a layer makes of one: the
    /// character the keyboard slice reads off the scancode and its layer.
    Char(char),
}

/// One row of the table: a verb of the list, the key that does it, and
/// the pane the key is pressed in.
#[derive(Clone, Copy, Debug)]
pub struct KeyRow {
    pub verb: &'static str,
    pub key: M9Key,
    pub pane: Pane,
}

/// The list's rows: move the cursor, open the conversation under it, and
/// back from it to the list.
pub const LIST_ROWS: [KeyRow; 4] = [
    KeyRow {
        verb: "up",
        key: M9Key::Up,
        pane: Pane::Conversations,
    },
    KeyRow {
        verb: "down",
        key: M9Key::Down,
        pane: Pane::Conversations,
    },
    KeyRow {
        verb: "open",
        key: M9Key::Enter,
        pane: Pane::Conversations,
    },
    KeyRow {
        verb: "back",
        key: M9Key::Back,
        pane: Pane::Messages,
    },
];

/// The chat's rows and the compose line's: move the cursor over the
/// bubbles, write, type, erase, move the caret, send, and back out of the
/// line to the bubbles. Typing is every character key; the row names one.
pub const CHAT_ROWS: [KeyRow; 9] = [
    KeyRow {
        verb: "up",
        key: M9Key::Up,
        pane: Pane::Messages,
    },
    KeyRow {
        verb: "down",
        key: M9Key::Down,
        pane: Pane::Messages,
    },
    KeyRow {
        verb: "write",
        key: M9Key::Char('i'),
        pane: Pane::Messages,
    },
    KeyRow {
        verb: "type",
        key: M9Key::Char('a'),
        pane: Pane::Compose,
    },
    KeyRow {
        verb: "erase",
        key: M9Key::Backspace,
        pane: Pane::Compose,
    },
    KeyRow {
        verb: "caret left",
        key: M9Key::Left,
        pane: Pane::Compose,
    },
    KeyRow {
        verb: "caret right",
        key: M9Key::Right,
        pane: Pane::Compose,
    },
    KeyRow {
        verb: "send",
        key: M9Key::Enter,
        pane: Pane::Compose,
    },
    KeyRow {
        verb: "cancel",
        key: M9Key::Back,
        pane: Pane::Compose,
    },
];

/// The key event `key` is: the terminal's key for the same verb, so the
/// shared table resolves it as it resolves the terminal's.
pub fn event(key: M9Key) -> KeyEvent {
    let code = match key {
        M9Key::Up => KeyCode::Up,
        M9Key::Down => KeyCode::Down,
        M9Key::Left => KeyCode::Left,
        M9Key::Right => KeyCode::Right,
        M9Key::Enter => KeyCode::Enter,
        M9Key::Back => KeyCode::Esc,
        M9Key::Backspace => KeyCode::Backspace,
        M9Key::Char(c) => KeyCode::Char(c),
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::keymap::{Ctx, ScrollCmd};
    use loquax::keys::{panes_of, resolve};
    use loquax::ui::intent::{Focus, Intent, Target};

    /// Each row's key, through the shared table and `Intent::from_action`,
    /// is the verb it names: the M9 adds no key binding of its own.
    #[test]
    fn the_list_rows_resolve_to_their_intents_through_the_shared_table() {
        let want = [
            Intent::Scroll(ScrollCmd::LineUp),
            Intent::Scroll(ScrollCmd::LineDown),
            Intent::Focus(Focus::Messages),
            Intent::Back,
        ];
        for (row, want) in LIST_ROWS.iter().zip(want) {
            let action = resolve(&event(row.key), panes_of(row.pane), Ctx::NONE)
                .unwrap_or_else(|| panic!("{} ({:?}) resolves to nothing", row.verb, row.key));
            assert_eq!(Intent::from_action(action), want, "{}", row.verb);
        }
    }

    /// The chat's rows the same way. A row the table leaves unbound in the
    /// compose line (`None`) is the field's: the model hands the key to it.
    #[test]
    fn the_chat_rows_resolve_to_their_intents_through_the_shared_table() {
        let want: [Option<Intent>; 9] = [
            Some(Intent::Scroll(ScrollCmd::LineUp)),
            Some(Intent::Scroll(ScrollCmd::LineDown)),
            Some(Intent::Focus(Focus::Compose)),
            None,
            None,
            Some(Intent::Edit),
            Some(Intent::Edit),
            Some(Intent::Send {
                to: Target::Open,
                body: None,
            }),
            Some(Intent::Back),
        ];
        for (row, want) in CHAT_ROWS.iter().zip(want) {
            let got =
                resolve(&event(row.key), panes_of(row.pane), Ctx::NONE).map(Intent::from_action);
            assert_eq!(got, want, "{} ({:?})", row.verb, row.key);
        }
    }

    /// The positive control: a key the table does not bind in the pane
    /// resolves to nothing, so the test above can fail.
    #[test]
    fn a_key_the_pane_does_not_bind_resolves_to_nothing() {
        assert!(resolve(
            &event(M9Key::Back),
            panes_of(Pane::Conversations),
            Ctx::NONE
        )
        .is_none());
    }
}
