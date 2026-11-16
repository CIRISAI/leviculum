//! The programs' own key and mouse events, and the text-editing table over
//! them.
//!
//! Shaped like crossterm's, field for field and variant for variant, because
//! the terminal frontend is the first translator and a one-to-one mapping is
//! the one that cannot drift. Owning them in this terminal-free crate is what
//! keeps a program's core free of the terminal crate: a GUI frontend builds
//! the same values from its own input and the core cannot tell the
//! difference. Positions are character cells, the unit the layout is measured
//! in (see [`crate::geometry`]). The crossterm translation lives in
//! `leviculum-ui-ratatui`.
//!
//! A text field a core holds is an [`InputBuffer`]: the trait is here and
//! `no_std`, its implementation over `tui_input::Input` comes with `std`, so
//! a messenger core built for a board names the field without the std-only
//! editor and a board brings its own. The command palette's names are
//! [`crate::commands`].

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{BitOr, BitOrAssign};

#[cfg(feature = "std")]
use tui_input::InputRequest;

/// A key, as the programs understand it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Backspace.
    Backspace,
    /// Enter.
    Enter,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Tab.
    Tab,
    /// Shift-Tab.
    BackTab,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// A function key, `F(1)` for F1.
    F(u8),
    /// A character key.
    Char(char),
    /// The null key.
    Null,
    /// Escape.
    Esc,
    /// Any other key a frontend can report (lock keys, media keys, a bare
    /// modifier). No binding names one; it exists so such a key still reaches
    /// a program's key handler as "some other key", exactly as it did when
    /// lnomad's core matched on crossterm's own codes.
    Other,
}

/// The modifier keys held with a key or mouse event, a bit set.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    /// No modifier.
    pub const NONE: Self = Self(0);
    /// Shift.
    pub const SHIFT: Self = Self(0b0000_0001);
    /// Control.
    pub const CONTROL: Self = Self(0b0000_0010);
    /// Alt (Option on a Mac).
    pub const ALT: Self = Self(0b0000_0100);
    /// Super (the Windows or Command key).
    pub const SUPER: Self = Self(0b0000_1000);
    /// Hyper.
    pub const HYPER: Self = Self(0b0001_0000);
    /// Meta.
    pub const META: Self = Self(0b0010_0000);

    /// The raw bit set, in crossterm's bit order.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The set with these raw bits; unknown bits are dropped.
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & 0b0011_1111)
    }

    /// Whether every modifier in `other` is held.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no modifier is held.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::fmt::Debug for KeyModifiers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        const NAMES: [(KeyModifiers, &str); 6] = [
            (KeyModifiers::SHIFT, "SHIFT"),
            (KeyModifiers::CONTROL, "CONTROL"),
            (KeyModifiers::ALT, "ALT"),
            (KeyModifiers::SUPER, "SUPER"),
            (KeyModifiers::HYPER, "HYPER"),
            (KeyModifiers::META, "META"),
        ];
        let held: Vec<&str> = NAMES
            .iter()
            .filter(|(m, _)| self.contains(*m))
            .map(|(_, name)| *name)
            .collect();
        match held.is_empty() {
            true => write!(f, "KeyModifiers(NONE)"),
            false => write!(f, "KeyModifiers({})", held.join(" | ")),
        }
    }
}

/// Whether a key went down, is auto-repeating, or came up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyEventKind {
    /// Pressed.
    Press,
    /// Held and repeating.
    Repeat,
    /// Released. Only some terminals report it.
    Release,
}

/// One key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The key.
    pub code: KeyCode,
    /// The modifiers held with it.
    pub modifiers: KeyModifiers,
    /// Press, repeat or release.
    pub kind: KeyEventKind,
}

impl KeyEvent {
    /// A key press.
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }
}

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Left button.
    Left,
    /// Right button.
    Right,
    /// Middle button.
    Middle,
    /// The back side button (button 8), history back.
    Back,
    /// The forward side button (button 9), history forward.
    Forward,
}

/// What the mouse did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseEventKind {
    /// A button went down.
    Down(MouseButton),
    /// A button came up.
    Up(MouseButton),
    /// The pointer moved with a button held.
    Drag(MouseButton),
    /// The pointer moved with no button held.
    Moved,
    /// The wheel turned towards the reader.
    ScrollDown,
    /// The wheel turned away from the reader.
    ScrollUp,
    /// The wheel tilted left.
    ScrollLeft,
    /// The wheel tilted right.
    ScrollRight,
}

/// One mouse event, at a character cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    /// What happened.
    pub kind: MouseEventKind,
    /// The cell column, from 0 at the left edge.
    pub column: u16,
    /// The cell row, from 0 at the top edge.
    pub row: u16,
    /// The modifiers held.
    pub modifiers: KeyModifiers,
}

/// A text field as a core holds it: what a key does to it, and what it holds.
/// loquax's compose pane is one; it holds more than one line once
/// [`InputBuffer::newline`] has been called, and every other edit treats the
/// line break as one character.
pub trait InputBuffer {
    /// The text.
    fn value(&self) -> &str;
    /// The cursor, in characters from the start.
    fn cursor(&self) -> usize;
    /// Apply what `key` asks of the text. `false` when the key is not an
    /// edit at all, so the caller may bind it to something else.
    fn edit(&mut self, key: &KeyEvent) -> bool;
    /// Empty the field and hand back what it held.
    fn take(&mut self) -> String;
    /// Insert a line break at the cursor. Not an [`InputBuffer::edit`] key:
    /// which key means "newline" is the caller's keymap (loquax's compose
    /// pane binds `Alt-Enter`, where `Enter` sends), not the field's.
    fn newline(&mut self);
    /// Put the cursor before the `at`th character, or at the end when the
    /// text is shorter: where a click in the field lands.
    fn set_cursor(&mut self, at: usize);
    /// Make the field hold `text`, the cursor at its end: a draft put back
    /// when its conversation opens again.
    fn set_value(&mut self, text: &str);
}

/// The display column of a field's cursor: the width of the characters
/// before it, a wide character counting two. What `tui_input::Input`'s
/// `visual_cursor` answers, over any [`InputBuffer`], so a core that draws a
/// caret names no editor.
pub fn visual_cursor(field: &impl InputBuffer) -> usize {
    let value = field.value();
    let end = value
        .char_indices()
        .nth(field.cursor())
        .map_or(value.len(), |(at, _)| at);
    unicode_width::UnicodeWidthStr::width(&value[..end])
}

/// How many display columns of a field to scroll past so its cursor stays
/// inside `width` columns, measured in whole characters from the start.
/// `tui_input::Input`'s `visual_scroll`, over any [`InputBuffer`].
pub fn visual_scroll(field: &impl InputBuffer, width: usize) -> usize {
    let scroll = visual_cursor(field).max(width) - width;
    let mut skipped = 0;
    let mut chars = field.value().chars();
    while skipped < scroll {
        match chars.next() {
            Some(c) => skipped += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0),
            None => break,
        }
    }
    skipped
}

/// A field that holds its text and cursor and edits nothing by key: the
/// field of a core whose platform brings its own text field (a phone's, a
/// window's, a board's touch keyboard) and hands the text in with
/// [`InputBuffer::set_value`]. `no_std`, so a core built for a board names a
/// field without the terminal's editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    text: String,
    cursor: usize,
}

impl InputBuffer for Line {
    fn value(&self) -> &str {
        &self.text
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn edit(&mut self, _key: &KeyEvent) -> bool {
        false
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        core::mem::take(&mut self.text)
    }

    fn newline(&mut self) {
        let at = self
            .text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(at, _)| at);
        self.text.insert(at, '\n');
        self.cursor += 1;
    }

    fn set_cursor(&mut self, at: usize) {
        self.cursor = at.min(self.text.chars().count());
    }

    fn set_value(&mut self, text: &str) {
        self.text = String::from(text);
        self.cursor = self.text.chars().count();
    }
}

/// The terminal's field: `tui-input`, edited through [`input_request`], the
/// same table lnomad's address bar and form fields edit with.
#[cfg(feature = "std")]
impl InputBuffer for tui_input::Input {
    fn value(&self) -> &str {
        tui_input::Input::value(self)
    }

    fn cursor(&self) -> usize {
        tui_input::Input::cursor(self)
    }

    fn edit(&mut self, key: &KeyEvent) -> bool {
        match line_request(
            key,
            tui_input::Input::value(self),
            tui_input::Input::cursor(self),
        ) {
            Some(request) => {
                self.handle(request);
                true
            }
            None => false,
        }
    }

    fn take(&mut self) -> String {
        self.value_and_reset()
    }

    fn newline(&mut self) {
        self.handle(InputRequest::InsertChar('\n'));
    }

    fn set_cursor(&mut self, at: usize) {
        let end = tui_input::Input::value(self).chars().count();
        self.handle(InputRequest::SetCursor(at.min(end)));
    }

    fn set_value(&mut self, text: &str) {
        // The yank buffer stays: `Ctrl-y` still brings back what was cut.
        *self = core::mem::take(self).with_value(text.to_string());
    }
}

/// The text-editing request a key makes of a [`tui_input::Input`], if any.
///
/// `tui-input`'s crossterm backend table (its `to_input_request`, version
/// 0.15), restated over this module's types, with the Emacs keys it gets
/// wrong put right (`docs/src/concepts/loquax-keys-and-layout.md`, "What the
/// terminal can and cannot send"): `Alt-f`, `Alt-b` and `Alt-d` edit under
/// `ALT`, which is what a terminal reports without the enhancement protocol,
/// as well as `META`; `Alt-d` kills the word forward, not back; `Ctrl-d`
/// deletes the character forward. One table, so lnomad's address bar and
/// loquax's compose pane edit alike. Releases edit nothing.
///
/// A buffer that holds lines edits through [`line_request`], which puts the
/// line motions in front of this.
#[cfg(feature = "std")]
pub fn input_request(key: &KeyEvent) -> Option<InputRequest> {
    use InputRequest::*;
    use KeyCode::*;
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let none = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let alt = KeyModifiers::ALT;
    let meta = KeyModifiers::META;
    let mods = key.modifiers;
    match key.code {
        Backspace if mods == none => Some(DeletePrevChar),
        Char('h') if mods == ctrl => Some(DeletePrevChar),
        Delete if mods == none => Some(DeleteNextChar),
        Tab if mods == none => None,
        Left if mods == none => Some(GoToPrevChar),
        Char('b') if mods == ctrl => Some(GoToPrevChar),
        Left if mods == ctrl => Some(GoToPrevWord),
        Char('b') if mods == meta || mods == alt => Some(GoToPrevWord),
        Right if mods == none => Some(GoToNextChar),
        Char('f') if mods == ctrl => Some(GoToNextChar),
        Right if mods == ctrl => Some(GoToNextWord),
        Char('f') if mods == meta || mods == alt => Some(GoToNextWord),
        Char('u') if mods == ctrl => Some(DeleteLine),
        Char('w') if mods == ctrl => Some(DeletePrevWord),
        Char('d') if mods == meta || mods == alt => Some(DeleteNextWord),
        Char('d') if mods == ctrl => Some(DeleteNextChar),
        Backspace if mods == meta || mods == alt => Some(DeletePrevWord),
        Delete if mods == ctrl => Some(DeleteNextWord),
        Char('k') if mods == ctrl => Some(DeleteTillEnd),
        Char('y') if mods == ctrl => Some(Yank),
        Char('a') if mods == ctrl => Some(GoToStart),
        Home if mods == none => Some(GoToStart),
        Char('e') if mods == ctrl => Some(GoToEnd),
        End if mods == none => Some(GoToEnd),
        Char(c) if mods == none || mods == KeyModifiers::SHIFT || mods == ctrl | alt => {
            Some(InsertChar(c))
        }
        _ => None,
    }
}

/// The request a key makes of a buffer that may hold lines: the line motions
/// over `value` with the cursor at `cursor` (in characters), else
/// [`input_request`]'s.
///
/// `Ctrl-a`, `Ctrl-e`, `Home` and `End` go to the start and end of the
/// cursor's line, `Alt-<` and `Alt->` to the start and end of the buffer, and
/// `↑`, `↓`, `Ctrl-p` and `Ctrl-n` to the same column of the line above and
/// below (`None` on the first and last line, so the key is free for the
/// caller). On one line the start and end of the line are the buffer's, so a
/// single-line field edits exactly as [`input_request`] has it.
#[cfg(feature = "std")]
pub fn line_request(key: &KeyEvent, value: &str, cursor: usize) -> Option<InputRequest> {
    use InputRequest::*;
    use KeyCode::*;
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let none = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let alt = KeyModifiers::ALT;
    let mods = key.modifiers;
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |at| at + 1);
    let end = chars[cursor..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |at| cursor + at);
    let column = cursor - start;
    match key.code {
        Char('a') if mods == ctrl => Some(SetCursor(start)),
        Home if mods == none => Some(SetCursor(start)),
        Char('e') if mods == ctrl => Some(SetCursor(end)),
        End if mods == none => Some(SetCursor(end)),
        Char('<') if mods == alt || mods == alt | KeyModifiers::SHIFT => Some(GoToStart),
        Char('>') if mods == alt || mods == alt | KeyModifiers::SHIFT => Some(GoToEnd),
        Up if mods == none => line_above(&chars, start, column),
        Char('p') if mods == ctrl => line_above(&chars, start, column),
        Down if mods == none => line_below(&chars, end, column),
        Char('n') if mods == ctrl => line_below(&chars, end, column),
        _ => input_request(key),
    }
}

/// The cursor at `column` of the line that ends just before `start`.
#[cfg(feature = "std")]
fn line_above(chars: &[char], start: usize, column: usize) -> Option<InputRequest> {
    let above_end = start.checked_sub(1)?;
    let above_start = chars[..above_end]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |at| at + 1);
    Some(InputRequest::SetCursor(
        above_start + column.min(above_end - above_start),
    ))
}

/// The cursor at `column` of the line that starts just after `end`.
#[cfg(feature = "std")]
fn line_below(chars: &[char], end: usize, column: usize) -> Option<InputRequest> {
    if end >= chars.len() {
        return None;
    }
    let below_start = end + 1;
    let below_end = chars[below_start..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |at| below_start + at);
    Some(InputRequest::SetCursor(
        below_start + column.min(below_end - below_start),
    ))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    /// The line break goes in at the cursor, and the field's other edits
    /// treat it as the one character it is: a backspace right after it
    /// joins the lines again.
    #[test]
    fn a_newline_goes_in_at_the_cursor_and_is_one_character() {
        let mut field = tui_input::Input::default();
        for c in "onetwo".chars() {
            field.edit(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        for _ in 0..3 {
            field.edit(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        field.newline();
        assert_eq!(InputBuffer::value(&field), "one\ntwo");
        assert_eq!(InputBuffer::cursor(&field), 4);
        field.edit(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(field.take(), "onetwo");
    }

    fn field(text: &str, cursor: usize) -> tui_input::Input {
        tui_input::Input::new(text.to_string()).with_cursor(cursor)
    }

    fn press(field: &mut tui_input::Input, code: KeyCode, mods: KeyModifiers) {
        field.edit(&KeyEvent::new(code, mods));
    }

    /// The Emacs word keys under `ALT`, as a terminal without the
    /// enhancement protocol reports them, and under `META`: `Alt-d` kills the
    /// word forward, `Ctrl-d` the character forward.
    #[test]
    fn alt_word_keys_edit_and_the_d_keys_delete_forward() {
        for alt in [KeyModifiers::ALT, KeyModifiers::META] {
            let mut f = field("one two three", 0);
            // tui-input's word motion lands on the next word's start.
            press(&mut f, KeyCode::Char('f'), alt);
            assert_eq!(InputBuffer::cursor(&f), 4, "{alt:?}");
            press(&mut f, KeyCode::Char('d'), alt);
            assert_eq!(InputBuffer::value(&f), "one three", "{alt:?}");
            assert_eq!(InputBuffer::cursor(&f), 4, "{alt:?}");
            press(&mut f, KeyCode::Char('b'), alt);
            assert_eq!(InputBuffer::cursor(&f), 0, "{alt:?}");
        }
        let mut f = field("abc", 1);
        press(&mut f, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(InputBuffer::value(&f), "ac");
        assert_eq!(InputBuffer::cursor(&f), 1);
    }

    /// In a buffer of lines, `Ctrl-a`/`Ctrl-e`/`Home`/`End` are the line's
    /// ends, `Alt-<`/`Alt->` the buffer's, and the line motions keep the
    /// column where the next line has it.
    #[test]
    fn line_motions_in_a_buffer_of_lines() {
        let text = "first\nsecond line\nend";
        let ctrl = KeyModifiers::CONTROL;
        let none = KeyModifiers::NONE;
        let cases = [
            (9, KeyCode::Char('a'), ctrl, 6),
            (9, KeyCode::Home, none, 6),
            (9, KeyCode::Char('e'), ctrl, 17),
            (9, KeyCode::End, none, 17),
            (9, KeyCode::Char('<'), KeyModifiers::ALT, 0),
            (9, KeyCode::Char('>'), KeyModifiers::ALT, 21),
            (9, KeyCode::Char('p'), ctrl, 3),
            (9, KeyCode::Up, none, 3),
            (16, KeyCode::Char('n'), ctrl, 21),
            (2, KeyCode::Down, none, 8),
        ];
        for (from, code, mods, to) in cases {
            let mut f = field(text, from);
            press(&mut f, code, mods);
            assert_eq!(InputBuffer::cursor(&f), to, "{code:?} {mods:?} from {from}");
        }
        // No line above the first or below the last: the key is not an edit.
        let up = KeyEvent::new(KeyCode::Up, none);
        assert_eq!(line_request(&up, text, 2), None);
        let down = KeyEvent::new(KeyCode::Down, none);
        assert_eq!(line_request(&down, text, 19), None);
    }

    /// lnomad's address bar is one line, so the line rule changes nothing
    /// for it: every line-start and line-end key lands where the plain table
    /// puts it.
    #[test]
    fn on_one_line_the_line_rule_is_the_plain_table() {
        let text = "lxmf@abc:/page/index.mu";
        let ctrl = KeyModifiers::CONTROL;
        let none = KeyModifiers::NONE;
        for (code, mods) in [
            (KeyCode::Char('a'), ctrl),
            (KeyCode::Char('e'), ctrl),
            (KeyCode::Home, none),
            (KeyCode::End, none),
        ] {
            for at in [0, 5, text.len()] {
                let key = KeyEvent::new(code, mods);
                let mut plain = field(text, at);
                if let Some(request) = input_request(&key) {
                    plain.handle(request);
                }
                let mut lined = field(text, at);
                if let Some(request) = line_request(&key, text, at) {
                    lined.handle(request);
                }
                assert_eq!(
                    (plain.value(), plain.cursor()),
                    (lined.value(), lined.cursor()),
                    "{code:?} {mods:?} at {at}"
                );
            }
        }
    }

    /// The caret column and the scroll a core reads off any field are the
    /// terminal editor's own, wide characters and all, so a frontend that
    /// switches fields draws the caret where it was.
    #[test]
    fn the_visual_cursor_and_scroll_are_the_editors() {
        for text in [
            "",
            "abc",
            "lxmf@abc:/page/index.mu",
            "日本語のページ",
            "a日b本c",
        ] {
            for at in 0..=text.chars().count() {
                let editor = field(text, at);
                let mut line = Line::default();
                line.set_value(text);
                line.set_cursor(at);
                assert_eq!(
                    visual_cursor(&line),
                    editor.visual_cursor(),
                    "{text:?} at {at}"
                );
                for width in [0, 1, 3, 8] {
                    assert_eq!(
                        visual_scroll(&line, width),
                        editor.visual_scroll(width),
                        "{text:?} at {at} in {width}"
                    );
                }
            }
        }
    }
}

/// The platform's field without the terminal's editor.
#[cfg(test)]
mod line {
    use super::*;

    /// Keys edit nothing; the text comes in whole, the cursor at its end,
    /// and goes out whole.
    #[test]
    fn a_line_takes_its_text_whole_and_no_key() {
        let mut line = Line::default();
        assert!(!line.edit(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
        line.set_value("päge");
        assert_eq!((line.value(), line.cursor()), ("päge", 4));
        line.set_cursor(2);
        line.newline();
        assert_eq!((line.value(), line.cursor()), ("pä\nge", 3));
        line.set_cursor(99);
        assert_eq!(line.cursor(), 5);
        assert_eq!(line.take(), "pä\nge");
        assert_eq!((line.value(), line.cursor()), ("", 0));
    }
}
