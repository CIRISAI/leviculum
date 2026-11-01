//! The reader's word on a peer from the palette: `rename`, `block`,
//! `unblock`, and the `blocked` view (`docs/src/concepts/loquax-mailbox.md`,
//! section 2, "Blocking, and names from the palette").
//!
//! A blocked peer's messages arrive, are proven and are stored as any other
//! one's, so nothing on the wire tells it apart. The screen leaves its
//! conversation out of the list (and of the search and the map), counts
//! nothing of it unread and raises no toast for it ([`UiModel::shows`]).
//! The palette's `blocked` turns the list into the blocked conversations,
//! so nothing is lost, and `unblock` brings one back with its history.
//!
//! Every change is the whole of what the contacts file says about the peer
//! ([`Contact`]), handed to the shell as [`UiEffect::Contact`] to write.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_ui::input::{InputBuffer, KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::toast::ToastKind;

use super::{Palette, UiEffect, UiModel, NONE_OPEN};
use crate::names::{self, Contact, Trust};
use crate::ui::strings::{Lang, StrId};

/// What the open palette asks for instead of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The reader's name for this peer: `Enter` sets it, an empty line
    /// clears it.
    Rename([u8; 16]),
    /// A reaction to the message with id `target` in `peer`'s
    /// conversation: a digit picks a quick one, `Enter` sends what was
    /// typed ([`super::reactions`]).
    React { peer: [u8; 16], target: [u8; 32] },
    /// A friend's code or address (1263, finding 6), read by
    /// [`crate::code::read`]: its conversation opens, and with `name`
    /// the palette asks the reader's name for it next (`add contact`).
    Code { name: bool },
}

impl<B> UiModel<B> {
    /// Whether the list shows `peer`'s conversation: the blocked ones in the
    /// palette's `blocked` view, the others outside it.
    pub fn shows(&self, peer: &[u8; 16]) -> bool {
        (self.names.trust(peer) == Trust::Blocked) == self.show_blocked
    }

    /// The open conversation's peer, while one is open.
    fn open_peer(&mut self) -> Option<[u8; 16]> {
        let peer = self.conversations.get(self.selected).map(|c| c.peer);
        if peer.is_none() {
            self.toast(ToastKind::Info, "no conversation here".to_string());
        }
        peer
    }

    /// Keep the open conversation one the list shows: a hidden one gives
    /// way to the list's first, or to none when the list is empty.
    pub(super) fn settle(&mut self) {
        let Some(open) = self.conversations.get(self.selected) else {
            return;
        };
        if self.shows(&open.peer) {
            return;
        }
        match self.order().first().copied() {
            Some(first) => self.select(first),
            None => {
                if let Some(left) = self.conversations.get_mut(self.selected) {
                    left.new_rule = None;
                }
                self.selected = NONE_OPEN;
                self.scroll_top = None;
                self.selection = None;
                self.message_cursor = 0;
            }
        }
    }

    /// The palette's rows while it asks: what for, and what `Enter` does.
    pub fn ask_rows(&self, ask: Ask) -> Vec<String> {
        match ask {
            Ask::Rename(peer) => vec![
                format!("your name for {}", self.names.label(&peer)),
                "Enter sets it, empty clears it".to_string(),
            ],
            Ask::React { peer, target } => self.react_rows(peer, target),
            Ask::Code { .. } => vec![
                "your friend's code or address".to_string(),
                "Enter opens the conversation".to_string(),
            ],
        }
    }

    /// Take `contact` as the reader's word and ask the shell to write it.
    fn set_contact(&mut self, contact: Contact) -> Vec<UiEffect> {
        self.names.contacts.set(&contact);
        vec![UiEffect::Contact(contact)]
    }

    /// The palette's `block` and `unblock`.
    pub(super) fn set_blocked(&mut self, blocked: bool) -> Vec<UiEffect> {
        let Some(peer) = self.open_peer() else {
            return Vec::new();
        };
        let label = self.names.label(&peer);
        if self.names.contacts.of(&peer).blocked == blocked {
            let state = match blocked {
                true => "already blocked",
                false => "not blocked",
            };
            self.toast(ToastKind::Info, format!("{label} is {state}"));
            return Vec::new();
        }
        // Unblocking shows the conversation with its history, in the list
        // it is back in.
        if !blocked {
            self.show_blocked = false;
        }
        // On a client of a home node the block reaches the home's other
        // devices (1146).
        let state = crate::home::wire::PeerState {
            blocked,
            ..self.peer_state(&peer)
        };
        let effects = self.set_peer_state(peer, state);
        let text = match blocked {
            true => format!("blocked {label}: its messages are kept, :blocked lists them"),
            false => format!("unblocked {label}"),
        };
        self.toast(ToastKind::Info, text);
        effects
    }

    /// The palette's `blocked`: the blocked conversations, or the others.
    pub(super) fn toggle_blocked_view(&mut self) {
        self.show_blocked = !self.show_blocked;
        self.settle();
        let text = match (self.show_blocked, self.order().len()) {
            (true, 0) => "nothing is blocked".to_string(),
            (true, n) => format!("{n} blocked, :blocked returns"),
            (false, _) => "the conversations again".to_string(),
        };
        self.toast(ToastKind::Info, text);
    }

    /// Apply the rename the palette asked for: `typed`, trimmed, is the new
    /// petname, and nothing clears it. On a client of a home node the name
    /// goes to the home node too, which hands it to the home's other
    /// devices (1139).
    pub(super) fn rename(&mut self, peer: [u8; 16], typed: &str) -> Vec<UiEffect> {
        let typed = typed.trim();
        let petname = match typed.is_empty() {
            true => None,
            false => match names::sanitise(typed) {
                Some(name) => Some(name),
                None => {
                    self.toast(
                        ToastKind::Error,
                        "nothing visible is left of that name".to_string(),
                    );
                    return Vec::new();
                }
            },
        };
        let mut contact = self.names.contacts.of(&peer);
        if contact.petname == petname {
            return Vec::new();
        }
        let cleared = petname.is_none();
        contact.petname = petname.clone();
        let effects = self.set_contact(contact);
        if self.home_client {
            self.petnames.push((peer, petname));
        }
        let label = self.names.label(&peer);
        let text = match cleared {
            true => format!("no name of yours for {label}"),
            false => format!("named {label}"),
        };
        self.toast(ToastKind::Info, text);
        effects
    }
}

impl<B> UiModel<B> {
    /// A petname one of the home's devices set, pulled (1139): taken as
    /// the reader's word here and written to the contacts file, never
    /// handed back. A name that sanitises to nothing clears it.
    pub(super) fn home_petname(
        &mut self,
        peer: [u8; 16],
        petname: Option<String>,
    ) -> Vec<UiEffect> {
        let petname = petname.as_deref().and_then(names::sanitise);
        let mut contact = self.names.contacts.of(&peer);
        if contact.petname == petname {
            return Vec::new();
        }
        contact.petname = petname;
        self.set_contact(contact)
    }

    /// The petnames this device set, for the shell to hand the home node.
    pub fn take_petnames(&mut self) -> Vec<([u8; 16], Option<String>)> {
        core::mem::take(&mut self.petnames)
    }
}

impl<B: InputBuffer + Default> UiModel<B> {
    /// The palette's `rename`: the palette again, holding the peer's
    /// petname to edit.
    pub(super) fn ask_rename(&mut self) {
        let Some(peer) = self.open_peer() else {
            return;
        };
        let mut query = B::default();
        for ch in self.names.petname(&peer).unwrap_or_default().chars() {
            query.edit(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        self.palette = Some(Palette {
            query,
            selected: 0,
            ask: Some(Ask::Rename(peer)),
            drawings: false,
        });
    }

    /// `Enter` in a palette that asks: what was asked is done with the text.
    pub(super) fn answer_ask(&mut self, ask: Ask, typed: String) -> Vec<UiEffect> {
        match ask {
            Ask::Rename(peer) => self.rename(peer, &typed),
            Ask::React { peer, target } => self.react(peer, target, &typed),
            Ask::Code { name } => self.take_code(&typed, name),
        }
    }

    /// The palette's `new conversation` and `add contact`: ask for the
    /// friend's code.
    pub(super) fn ask_code(&mut self, name: bool) {
        self.palette = Some(Palette {
            ask: Some(Ask::Code { name }),
            ..Palette::default()
        });
    }

    /// A friend's code typed or pasted into the palette: the conversation
    /// open with the cursor in compose, then the name asked for when
    /// `name`. A code that is none is said in a toast, the same sentence
    /// the phone shows.
    fn take_code(&mut self, typed: &str, name: bool) -> Vec<UiEffect> {
        let code = match crate::code::read(typed) {
            Ok(code) => code,
            Err(why) => {
                self.toast(ToastKind::Error, why.say(Lang::En).to_string());
                return Vec::new();
            }
        };
        let Some(peer) = code.address() else {
            self.toast(
                ToastKind::Error,
                StrId::CodeIsMailbox.text(Lang::En).to_string(),
            );
            return Vec::new();
        };
        self.show_blocked = false;
        let index = self.conversation(peer);
        self.select(index);
        self.focus = super::Pane::Compose;
        if name {
            self.ask_rename();
        }
        Vec::new()
    }
}
