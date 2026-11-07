//! The window's text selection (1444): a double click on a bubble's words
//! selects the word, a triple click its line, a press and a drag a range
//! across bubbles in reading order, a Shift+click extends it, a plain
//! click or Escape clears it, Ctrl+A selects the whole conversation.
//! Every change puts the selected text on the PRIMARY selection (X11 and
//! Wayland, through iced's own clipboard, `window_clipboard`'s primary),
//! so a middle click elsewhere pastes it; Ctrl+C puts it on the
//! clipboard. A middle click in the compose field pastes the PRIMARY
//! selection there.
//!
//! The rules (the word, the line, the order of the runs, the copied text)
//! are the core's ([`loquax::ui::selection`]); the pointer's place in a
//! text is the shared widget's ([`leviculum_ui_iced::select`]). This file
//! only keeps the window's one [`TextSelection`] and turns it into
//! clipboard writes.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use iced::keyboard;
use iced::widget::text_editor;
use iced::Task;
use leviculum_ui_iced::select::Pick;
use loquax::ui::selection::{line_at, runs, word_at, Spot, TextRun, TextSelection};

use super::{App, Message, Screen};

/// Which of the system's two buffers a copy goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Buffer {
    /// What Ctrl+C copies and Ctrl+V pastes.
    Clipboard,
    /// What a selection is on X11 and Wayland, which a middle click pastes.
    Primary,
}

impl App {
    /// The open conversation's text runs, in reading order.
    pub(super) fn text_runs(&self) -> Vec<TextRun> {
        match &self.scene.chat {
            Some(chat) => runs(chat, self.lang),
            None => Vec::new(),
        }
    }

    /// The selected bytes of each run the selection touches, for the view:
    /// empty without a selection, at no cost to a long chat.
    pub(super) fn picked_ranges(&self) -> HashMap<u64, Range<usize>> {
        let Some(picked) = self.picked else {
            return HashMap::new();
        };
        let runs = self.text_runs();
        let Some((i, _, j, _)) = picked.ordered(&runs) else {
            return HashMap::new();
        };
        runs[i..=j]
            .iter()
            .filter_map(|run| Some((run.key, picked.range_in(&runs, run.key)?)))
            .collect()
    }

    /// `text` onto `buffer`. The tests read what went where from
    /// [`App::clips`], the test clipboard.
    fn clip(&mut self, buffer: Buffer, text: String) -> Task<Message> {
        #[cfg(test)]
        self.clips.push((buffer, text.clone()));
        match buffer {
            Buffer::Clipboard => iced::clipboard::write(text),
            Buffer::Primary => iced::clipboard::write_primary(text),
        }
    }

    /// The selection's text onto the PRIMARY selection, when it selects
    /// anything.
    fn onto_primary(&mut self, runs: &[TextRun]) -> Task<Message> {
        let text = self.picked.map(|p| p.text(runs)).unwrap_or_default();
        match text.is_empty() {
            true => Task::none(),
            false => self.clip(Buffer::Primary, text),
        }
    }

    /// The left button on the words of the bubble `key`.
    pub(super) fn pick_text(&mut self, key: u64, pick: Pick) -> Task<Message> {
        let runs = self.text_runs();
        let Some(run) = runs.iter().find(|r| r.key == key) else {
            return Task::none();
        };
        match pick {
            Pick::Press { at, clicks, shift } => {
                let spot = Spot { key, at };
                self.picking = true;
                self.picked = Some(match (clicks, shift, self.picked) {
                    (_, true, Some(picked)) => picked.extend(spot),
                    (2, _, _) => TextSelection::within(key, word_at(&run.text, at)),
                    (3, _, _) => TextSelection::within(key, line_at(&run.text, at)),
                    _ => TextSelection::caret(spot),
                });
                self.onto_primary(&runs)
            }
            Pick::Drag { at } if self.picking => {
                let before = self.picked;
                self.picked = self.picked.map(|p| p.extend(Spot { key, at }));
                match self.picked == before {
                    true => Task::none(),
                    false => self.onto_primary(&runs),
                }
            }
            Pick::Drag { .. } => Task::none(),
            Pick::Release => {
                self.picking = false;
                Task::none()
            }
        }
    }

    /// The selection's keys in a chat: Ctrl+C copies it, Ctrl+A selects
    /// the whole conversation, Escape clears it. `None` for any other key,
    /// and for these where they have nothing to do, which leaves the key
    /// to the keymap. The compose field's own Ctrl+C, Ctrl+A and Ctrl+V
    /// never come here: iced hands the window only the keys no widget
    /// took.
    pub(super) fn selection_key(&mut self, event: &keyboard::Event) -> Option<Task<Message>> {
        let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
            return None;
        };
        if self.screen != Screen::Chats || self.scene.chat.is_none() {
            return None;
        }
        let runs = self.text_runs();
        let selected = self.picked.filter(|p| !p.is_empty(&runs));
        match key.as_ref() {
            keyboard::Key::Character("c") if modifiers.command() => {
                let text = selected?.text(&runs);
                Some(self.clip(Buffer::Clipboard, text))
            }
            keyboard::Key::Character("a") if modifiers.command() => {
                self.picked = TextSelection::all(&runs);
                Some(self.onto_primary(&runs))
            }
            keyboard::Key::Named(keyboard::key::Named::Escape) => {
                selected?;
                self.picked = None;
                Some(Task::none())
            }
            _ => None,
        }
    }

    /// A middle click on the compose field: the PRIMARY selection is read
    /// and pasted there.
    pub(super) fn paste_primary(&self) -> Task<Message> {
        iced::clipboard::read_primary().map(Message::PrimaryText)
    }

    /// What the PRIMARY selection held, pasted at the compose field's
    /// cursor.
    pub(super) fn primary_text(&mut self, text: Option<String>) {
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            return;
        };
        self.compose
            .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new(text),
            )));
        self.tell_draft();
    }
}
