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

use alloc::collections::BTreeMap;
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
    /// The terminal's group picker (1307): a friend's name ticks it, an
    /// empty line goes on to [`Ask::GroupName`].
    GroupPick,
    /// The new group's name: `Enter` makes it.
    GroupName,
    /// A group's members, shown; `Enter` closes.
    Members([u8; 16]),
    /// Leave the group: `Enter` leaves.
    LeaveGroup([u8; 16]),
    /// "delete group" (1307 pass four): Enter deletes it for everyone.
    DeleteGroup([u8; 16]),
    /// A member's name: `Enter` removes it from the group.
    RemoveMember([u8; 16]),
    /// A friend's name (1371): `Enter` forwards the message `key` of
    /// `from`'s conversation there.
    Forward { from: [u8; 16], key: u64 },
    /// The own position by hand (1371): `Enter` sets it, alone the map's
    /// centre.
    Position,
    /// How long `peer`'s chat gets the position (1371): `Enter` alone
    /// once, now.
    Share([u8; 16]),
    /// "delete all maps" (1371): `Enter` deletes them.
    DeleteAllMaps,
    /// A path the shell reads (1371): `Enter` hands it over.
    Path(PathFor),
    /// Another hub for the public rooms (1577): `Enter` takes the address
    /// typed or pasted.
    RoomsHub,
}

/// What a typed path is for (1371): the terminal's picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathFor {
    /// A picture for `peer`'s chat, scaled at the photo size.
    Picture([u8; 16]),
    /// A file for `peer`'s chat, whole.
    File([u8; 16]),
    /// A picture of a friend's code: its conversation opens.
    Code,
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
        if let Some(rows) = self.verb_ask_rows(ask) {
            return rows;
        }
        match ask {
            Ask::Rename(peer) => vec![
                format!("your name for {}", self.names.label(&peer, self.lang)),
                "Enter sets it, empty clears it".to_string(),
            ],
            Ask::React { peer, target } => self.react_rows(peer, target),
            Ask::Code { .. } => vec![
                super::strings::StrId::AskContactCode
                    .text(self.lang)
                    .to_string(),
                "Enter opens the conversation".to_string(),
            ],
            Ask::RoomsHub => vec![
                super::strings::Text::key(super::strings::StrId::RoomsHubPasteHint)
                    .resolve(self.lang),
                super::strings::Text::key(super::strings::StrId::RoomsNameVisible)
                    .resolve(self.lang),
            ],
            group => self.group_ask_rows(group),
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
        let label = self.names.label(&peer, self.lang);
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
            // Owed until the home node takes it (1635): kept in the store,
            // handed again after a restart.
            self.owed_petnames.set(peer, petname);
        }
        let label = self.names.label(&peer, self.lang);
        let text = match cleared {
            true => format!("no name of yours for {label}"),
            false => format!("named {label}"),
        };
        self.toast(ToastKind::Info, text);
        effects
    }
}

impl<B> UiModel<B> {
    /// A petname one of the home's devices set, pulled (1139), and when
    /// the home took it (`at`, the home's clock): taken as the reader's
    /// word here and written to the contacts file, never handed back. A
    /// name that sanitises to nothing clears it.
    ///
    /// Not over a newer word of this device's (1635). While this device
    /// owes the home a petname for the peer, the home has not taken it,
    /// and once it does it is the home's newest: what the pull brought is
    /// older, a name the reader cleared among them. Once the home took it,
    /// the archive holds nothing older for the peer, but a pull already
    /// under way may: until that pull ends an entry counts only if the
    /// home took it after this device's (`at` past the time its answer
    /// came). An entry that says the same is the home's echo of it.
    pub(super) fn home_petname(
        &mut self,
        peer: [u8; 16],
        petname: Option<String>,
        at: i64,
    ) -> Vec<UiEffect> {
        let petname = petname.as_deref().and_then(names::sanitise);
        if !self.owed_petnames.pulled(&peer, &petname, at) {
            return Vec::new();
        }
        let mut contact = self.names.contacts.of(&peer);
        if contact.petname == petname {
            return Vec::new();
        }
        contact.petname = petname;
        self.set_contact(contact)
    }

    /// The petnames this device owes the home node, the newest of each
    /// peer, for the shell to hand it; it answers with
    /// [`super::UiEvent::PetnamesHanded`]. Nothing while a hand is under way, so
    /// the home takes a peer's names in the order they were set (1635):
    /// two hands at once could reach it the other way round.
    pub fn take_petnames(&mut self) -> Vec<([u8; 16], Option<String>)> {
        self.owed_petnames.take()
    }

    /// A hand ended (1635): what the home took is owed no more, unless the
    /// reader set another name for the peer meanwhile, and stands over
    /// older entries until the next pull ends. What it did not take goes
    /// with the next hand.
    pub(super) fn petnames_handed(&mut self, taken: Vec<([u8; 16], Option<String>)>) {
        let now = self.now.map_or(0, |at| at.unix_secs);
        self.owed_petnames.handed(taken, now);
    }

    /// The petnames the store kept as owed (1635), at the start: handed
    /// again with the first hand, and the reader's word over what the
    /// first pull brings.
    pub fn restore_owed_petnames(&mut self, owed: BTreeMap<[u8; 16], Option<String>>) {
        self.owed_petnames.restore(owed);
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
        if let Some(effects) = self.answer_verb_ask(ask, &typed) {
            return effects;
        }
        match ask {
            Ask::Rename(peer) => self.rename(peer, &typed),
            Ask::React { peer, target } => self.react(peer, target, &typed),
            Ask::Code { name } => self.take_code(&typed, name),
            Ask::RoomsHub => super::rooms::choose(self, super::rooms::HubChoice::Address(typed)),
            group => self.answer_group_ask(group, typed),
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
    pub(super) fn take_code(&mut self, typed: &str, name: bool) -> Vec<UiEffect> {
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
