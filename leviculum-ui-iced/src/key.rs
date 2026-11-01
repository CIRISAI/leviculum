//! A key iced reports, as the shared keymap's key
//! (`leviculum_ui::input::KeyEvent`), the twin of
//! `leviculum_ui_ratatui::event::key_event`: every window resolves its
//! chords through `leviculum_ui::keymap` as the terminals do.

use iced::keyboard;
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};

/// A key iced reports as the keymap's key (1218): the named keys the
/// chords use, a character as itself, the modifiers held. None for a
/// release or a key no chord names.
pub fn chord(event: &keyboard::Event) -> Option<KeyEvent> {
    use keyboard::key::Named;
    let keyboard::Event::KeyPressed {
        modified_key,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    let shift = modifiers.shift();
    let code = match modified_key.as_ref() {
        keyboard::Key::Named(named) => match named {
            Named::Escape => KeyCode::Esc,
            Named::Enter => KeyCode::Enter,
            Named::Tab if shift => KeyCode::BackTab,
            Named::Tab => KeyCode::Tab,
            Named::Backspace => KeyCode::Backspace,
            Named::Delete => KeyCode::Delete,
            Named::Insert => KeyCode::Insert,
            Named::ArrowLeft => KeyCode::Left,
            Named::ArrowRight => KeyCode::Right,
            Named::ArrowUp => KeyCode::Up,
            Named::ArrowDown => KeyCode::Down,
            Named::Home => KeyCode::Home,
            Named::End => KeyCode::End,
            Named::PageUp => KeyCode::PageUp,
            Named::PageDown => KeyCode::PageDown,
            Named::Space => KeyCode::Char(' '),
            Named::F1 => KeyCode::F(1),
            Named::F2 => KeyCode::F(2),
            Named::F3 => KeyCode::F(3),
            Named::F4 => KeyCode::F(4),
            Named::F5 => KeyCode::F(5),
            Named::F6 => KeyCode::F(6),
            Named::F7 => KeyCode::F(7),
            Named::F8 => KeyCode::F(8),
            Named::F9 => KeyCode::F(9),
            Named::F10 => KeyCode::F(10),
            Named::F11 => KeyCode::F(11),
            Named::F12 => KeyCode::F(12),
            _ => return None,
        },
        keyboard::Key::Character(c) => {
            let mut chars = c.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
        keyboard::Key::Unidentified => return None,
    };
    let mut held = KeyModifiers::NONE;
    if shift {
        held |= KeyModifiers::SHIFT;
    }
    if modifiers.control() {
        held |= KeyModifiers::CONTROL;
    }
    if modifiers.alt() {
        held |= KeyModifiers::ALT;
    }
    Some(KeyEvent::new(code, held))
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyboard::key::{Named, NativeCode, Physical};

    fn press(key: keyboard::Key, modifiers: keyboard::Modifiers) -> keyboard::Event {
        keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        }
    }

    /// Shift-Tab is the back tab, a character keeps its modifiers, a key
    /// no chord names is none.
    #[test]
    fn a_key_is_the_keymaps_key() {
        let back = press(keyboard::Key::Named(Named::Tab), keyboard::Modifiers::SHIFT);
        assert_eq!(
            chord(&back),
            Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))
        );
        let ctrl_l = press(
            keyboard::Key::Character("l".into()),
            keyboard::Modifiers::CTRL,
        );
        assert_eq!(
            chord(&ctrl_l),
            Some(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL))
        );
        let caps = press(
            keyboard::Key::Named(Named::CapsLock),
            keyboard::Modifiers::empty(),
        );
        assert_eq!(chord(&caps), None);
    }
}
