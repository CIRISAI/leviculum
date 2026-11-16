//! The compose line's field on the M9: the text and its cursor, edited by
//! the keys a 37-key keyboard sends.
//!
//! The terminal's field is `tui_input::Input`, edited through
//! `leviculum_ui::input::line_request`; both are `std`. The phone's and the
//! window's fields are the platform's (`leviculum_ui::input::Line`, which
//! edits nothing by key). The M9 has no platform field, so this one edits:
//! a character in at the cursor, Backspace and Delete, the cursor left and
//! right, Home and End. Those are the edits the board's keys reach; the
//! Emacs chords and the kill buffer need a Control and an Alt the
//! keyboard does not have (`thinknode-m9.md`, "Keyboard"). Every other key
//! is not an edit, so the model binds it as it binds the terminal's.

use alloc::string::String;

use leviculum_ui::input::{InputBuffer, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The M9's compose field: its text and the cursor, in characters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    text: String,
    cursor: usize,
}

impl Field {
    /// The byte offset of character `at`, or the end.
    fn byte(&self, at: usize) -> usize {
        self.text
            .char_indices()
            .nth(at)
            .map_or(self.text.len(), |(byte, _)| byte)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }
}

impl InputBuffer for Field {
    fn value(&self) -> &str {
        &self.text
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn edit(&mut self, key: &KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        let plain = key.modifiers == KeyModifiers::NONE;
        match key.code {
            KeyCode::Char(c) if plain || key.modifiers == KeyModifiers::SHIFT => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if plain => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let at = self.byte(self.cursor);
                    self.text.remove(at);
                }
            }
            KeyCode::Delete if plain => {
                if self.cursor < self.len() {
                    let at = self.byte(self.cursor);
                    self.text.remove(at);
                }
            }
            KeyCode::Left if plain => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right if plain => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home if plain => self.cursor = 0,
            KeyCode::End if plain => self.cursor = self.len(),
            _ => return false,
        }
        true
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        core::mem::take(&mut self.text)
    }

    fn newline(&mut self) {
        let at = self.byte(self.cursor);
        self.text.insert(at, '\n');
        self.cursor += 1;
    }

    fn set_cursor(&mut self, at: usize) {
        self.cursor = at.min(self.len());
    }

    fn set_value(&mut self, text: &str) {
        self.text = String::from(text);
        self.cursor = self.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(field: &mut Field, code: KeyCode) -> bool {
        field.edit(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn keys_edit_at_the_cursor() {
        let mut field = Field::default();
        for c in "Hallo".chars() {
            assert!(press(&mut field, KeyCode::Char(c)));
        }
        assert!(press(&mut field, KeyCode::Left));
        assert!(press(&mut field, KeyCode::Left));
        assert!(press(&mut field, KeyCode::Backspace));
        assert!(press(&mut field, KeyCode::Char('ö')));
        assert_eq!((field.value(), field.cursor()), ("Haölo", 3));
        assert!(press(&mut field, KeyCode::Delete));
        assert!(press(&mut field, KeyCode::End));
        assert!(press(&mut field, KeyCode::Char('!')));
        assert_eq!((field.value(), field.cursor()), ("Haöo!", 5));
        assert!(press(&mut field, KeyCode::Home));
        assert!(
            press(&mut field, KeyCode::Backspace),
            "an edit, at the start a no-op"
        );
        assert_eq!((field.value(), field.cursor()), ("Haöo!", 0));
        assert_eq!(field.take(), "Haöo!");
        assert_eq!((field.value(), field.cursor()), ("", 0));
    }

    /// The positive control: a key that is no edit leaves the field and
    /// says so, so the model may bind it.
    #[test]
    fn a_key_that_is_no_edit_is_left_to_the_model() {
        let mut field = Field::default();
        field.set_value("ab");
        assert!(!press(&mut field, KeyCode::Enter));
        assert!(!press(&mut field, KeyCode::Esc));
        assert!(!press(&mut field, KeyCode::Up));
        assert!(!field.edit(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert_eq!((field.value(), field.cursor()), ("ab", 2));
    }
}
