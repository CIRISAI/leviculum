//! The model's side of the history (`docs/src/concepts/loquax-storage.md`;
//! `loquax-keys-and-layout.md`, section 7, slice 6): what the store hands
//! back at start, what the model asks it to write, the earlier pages of a
//! conversation, local delete and pinning.
//!
//! The store itself is the shell's (`crate::store`, SQLite, behind the
//! `history` feature): the model knows nothing of files. It keeps a note of
//! every message and conversation a fold changed, and the shell takes them
//! after each fold ([`UiModel::take_writes`]) and writes them in one
//! transaction. A message's key in the store is its [`Message::activity`],
//! which the model counts on from the largest key the store held, so a key
//! is never given twice across runs.
//!
//! # The restart, said honestly
//!
//! A send in flight when loquax ended is not in flight after the restart:
//! the router that held it is gone, and nothing sends it again. So a
//! message the store holds as queued or in flight comes back `○`, queued,
//! with a ledger line saying why ([`RESTARTED`]), and stays so: the reader
//! sees it was never proven, and can send it again.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::input::InputBuffer;
use leviculum_ui::toast::{Toast, ToastKind};

use super::{Message, NewRule, UiEffect, UiModel};
use crate::home::wire::MarkFlag;
use crate::telemetry::Fix;
use crate::when::WallTime;

/// How many of a conversation's newest messages the screen loads at start,
/// and how many more each time the reader reaches the top of what is loaded:
/// many screens, and an index lookup either way (storage page, "On the 2 GB
/// Raspberry Pi question").
pub const LOAD_WINDOW: usize = 200;

/// The ledger line of a send the restart cut off.
pub const RESTARTED: &str = "Loquax restarted: no longer being sent";

/// What a conversation keeps of the reader's word besides its pin (1146):
/// muted until when, archived, and the peer's identity verified in person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// Unix seconds the mute ends, `i64::MAX` for always.
    pub muted_until: Option<i64>,
    pub archived: bool,
    pub verified: bool,
}

/// How long typing must pause before a draft is written: a keystroke
/// moves the write on by this much (1219).
pub const DRAFT_DEBOUNCE_MS: u64 = 500;

/// A message typed and not sent, one a conversation (1219): kept in the
/// store beside its conversation, put back into compose when it opens
/// again, gone once sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub text: String,
    /// The message a reply being written answers, by its LXMF id.
    pub reply: Option<[u8; 32]>,
    /// What is to go with it; empty until messages carry attachments.
    pub attachments: Vec<String>,
}

impl Draft {
    /// Whether there is nothing to keep.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.reply.is_none() && self.attachments.is_empty()
    }

    /// The list row's words for it (`Draft: …`): the text on one line,
    /// none while it holds no text.
    pub fn line(&self) -> Option<String> {
        let line = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        (!line.is_empty()).then_some(line)
    }
}

/// One conversation as the store hands it back at start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restored {
    pub peer: [u8; 16],
    pub pinned: bool,
    pub flags: Flags,
    /// Received messages not yet looked at when loquax ended.
    pub unread: usize,
    /// The newest [`LOAD_WINDOW`] messages, oldest first.
    pub messages: Vec<Message>,
    /// How many the store holds before them.
    pub earlier: usize,
    /// What was typed and not sent.
    pub draft: Draft,
}

/// One change for the store to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreWrite {
    /// Write this message, its ledger with it, under its key.
    Message { peer: [u8; 16], message: Message },
    /// Write the conversation's own state.
    Conversation {
        peer: [u8; 16],
        pinned: bool,
        unread: usize,
        flags: Flags,
    },
    /// Delete the message with this key from the device.
    ForgetMessage { key: u64 },
    /// Delete the message with this LXMF id from the device, loaded or not:
    /// another device of the home deleted it.
    ForgetId { id: [u8; 32] },
    /// Delete the conversation and every message in it from the device,
    /// the positions it measured with them.
    ForgetConversation { peer: [u8; 16] },
    /// Keep one received position; one already there for the same peer and
    /// fix time is the same fix, arrived again.
    Position(Fix),
    /// Keep the conversation's draft as it stands, the empty one deleting
    /// it (1219).
    Draft { peer: [u8; 16], draft: Draft },
}

/// A message the store found for a search and the screen has not loaded:
/// older than what its conversation shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredHit {
    pub peer: [u8; 16],
    /// Its key, [`Message::activity`].
    pub key: u64,
    pub outbound: bool,
    pub body: String,
    pub at: Option<WallTime>,
}

/// What `y` would delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    Message { peer: [u8; 16], key: u64 },
    Conversation { peer: [u8; 16] },
}

/// What the screen asks the store besides writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreRead {
    /// The [`LOAD_WINDOW`] messages before `before` in `peer`'s
    /// conversation, or every one from `through` on when that reaches
    /// further: answered with [`super::UiEvent::Earlier`].
    Earlier {
        peer: [u8; 16],
        before: u64,
        through: Option<u64>,
    },
    /// The messages the screen has not loaded whose text has `query` in
    /// it: answered with [`super::UiEvent::StoredHits`].
    Search(String),
}

/// A send still being worked on at exit, as the store holds it.
fn cut_off(message: &Message) -> bool {
    message.outbound
        && matches!(
            message.delivery,
            Some(DeliveryState::Queued | DeliveryState::Stamping | DeliveryState::InFlight)
        )
}

impl<B: InputBuffer> UiModel<B> {
    /// The store's conversations at start, merged into those `--to` listed:
    /// each with its newest messages, its pin and its unread count, a send
    /// the restart cut off back to `○` with [`RESTARTED`] in its ledger. The
    /// open conversation is the first `--to` one, or else the list's first,
    /// at its newest message.
    pub fn restore(&mut self, restored: Vec<Restored>) {
        let listed = !self.conversations.is_empty();
        for stored in restored {
            self.restore_one(stored);
        }
        let open = match listed {
            true => 0,
            false => self.order().first().copied().unwrap_or(0),
        };
        self.selected = open;
        self.read_out(open);
        if let Some(conversation) = self.conversations.get(open) {
            self.message_cursor = conversation.messages.len().saturating_sub(1);
        }
        // A `--to` peer the reader blocked is not the one to open.
        self.settle();
        self.off_reactions();
        // The open conversation's draft is in compose before the first
        // key (1219).
        self.sync_compose();
    }
}

impl<B> UiModel<B> {
    /// One conversation the store handed back, listed as at start.
    fn restore_one(&mut self, stored: Restored) {
        if stored.messages.is_empty()
            && !stored.pinned
            && stored.flags == Flags::default()
            && stored.draft.is_empty()
        {
            return;
        }
        let now = self.now;
        let index = self.conversation(stored.peer);
        let conversation = &mut self.conversations[index];
        conversation.messages = stored.messages;
        conversation.pinned = stored.pinned;
        conversation.flags = stored.flags;
        conversation.earlier = stored.earlier;
        conversation.unread = stored.unread;
        conversation.draft = stored.draft;
        conversation.new_rule = conversation.rule_from_unread();
        conversation.activity = conversation.list_activity();
        for message in &mut conversation.messages {
            if cut_off(message) {
                message.delivery = Some(DeliveryState::Queued);
                message.note(now, RESTARTED);
                self.changed.insert((stored.peer, message.activity));
            }
            message.leg = None;
        }
        self.activity = self.activity.max(conversation.activity);
    }

    /// History the store gained under the screen while it ran: the rows a
    /// pull imported (1159), which take keys below every key the store
    /// held. `stored` is the store's conversations as at start; `below`
    /// answers what the store holds before a key of a conversation, as
    /// [`StoreRead::Earlier`] is answered. A conversation the screen does
    /// not list comes as at start. One loaded down to its oldest message
    /// gets the rows below it above what it shows; one with more still in
    /// the store learns how many more, and loads them as the reader
    /// scrolls. Nothing opens, nothing is read out, the reader stays where
    /// they were.
    pub fn grown<E>(
        &mut self,
        stored: Vec<Restored>,
        mut below: impl FnMut([u8; 16], u64) -> Result<(Vec<Message>, usize), E>,
    ) -> Result<(), E>
    where
        B: InputBuffer,
    {
        for stored in stored {
            let Some(c) = self
                .conversations
                .iter()
                .position(|c| c.peer == stored.peer)
            else {
                self.restore_one(stored);
                continue;
            };
            let conversation = &self.conversations[c];
            let (Some(first), earlier) = (conversation.messages.first(), conversation.earlier)
            else {
                self.prepend(stored.peer, stored.messages, stored.earlier);
                let conversation = &mut self.conversations[c];
                conversation.activity = conversation.activity.max(conversation.list_activity());
                continue;
            };
            let (older, more) = below(stored.peer, first.activity)?;
            match earlier {
                0 => self.prepend(stored.peer, older, more),
                _ => self.conversations[c].earlier = older.len() + more,
            }
        }
        Ok(())
    }

    /// Everything a fold changed that the store must learn, in the order to
    /// write it: the messages and conversations as they stand now, then the
    /// deletions. A message deleted after it changed is only deleted.
    pub fn take_writes(&mut self) -> Vec<StoreWrite> {
        let mut writes = Vec::new();
        for (peer, key) in core::mem::take(&mut self.changed) {
            let found = self
                .conversations
                .iter()
                .find(|c| c.peer == peer)
                .and_then(|c| c.messages.iter().find(|m| m.activity == key));
            if let Some(message) = found {
                writes.push(StoreWrite::Message {
                    peer,
                    message: message.clone(),
                });
            }
        }
        for peer in core::mem::take(&mut self.changed_peers) {
            if let Some(c) = self.conversations.iter().find(|c| c.peer == peer) {
                writes.push(StoreWrite::Conversation {
                    peer,
                    pinned: c.pinned,
                    unread: c.unread,
                    flags: c.flags,
                });
            }
        }
        // After the messages: a position names the row of its message.
        writes.extend(
            core::mem::take(&mut self.fixes_received)
                .into_iter()
                .map(StoreWrite::Position),
        );
        // A draft once typing paused (1219), every one when leaving.
        let now = self.elapsed_ms;
        let quit = self.quit;
        let due: Vec<[u8; 16]> = self
            .drafts_due
            .iter()
            .filter(|&(_, &at)| quit || at <= now)
            .map(|(&peer, _)| peer)
            .collect();
        for peer in due {
            self.drafts_due.remove(&peer);
            if let Some(c) = self.conversations.iter().find(|c| c.peer == peer) {
                writes.push(StoreWrite::Draft {
                    peer,
                    draft: c.draft.clone(),
                });
            }
        }
        writes.append(&mut self.forgotten);
        writes
    }

    /// Make every draft not yet written due now: the shell is about to end
    /// the session.
    pub fn flush_drafts(&mut self) {
        for at in self.drafts_due.values_mut() {
            *at = 0;
        }
    }

    /// Note that `peer`'s draft changed: written once typing paused for
    /// [`DRAFT_DEBOUNCE_MS`], or at once.
    pub(super) fn draft_changed(&mut self, peer: [u8; 16], at_once: bool) {
        let at = match at_once {
            true => 0,
            false => self.elapsed_ms.saturating_add(DRAFT_DEBOUNCE_MS),
        };
        self.drafts_due.insert(peer, at);
    }

    /// Note that message `m` of conversation `c` changed.
    pub(super) fn touch(&mut self, c: usize, m: usize) {
        if let Some(conversation) = self.conversations.get(c) {
            if let Some(message) = conversation.messages.get(m) {
                self.changed.insert((conversation.peer, message.activity));
            }
        }
    }

    /// Put `older` above what `peer`'s conversation shows, with `earlier`
    /// still in the store before them. Every index into the conversation
    /// moves with it, and a reader scrolled up keeps the rows they were
    /// reading where they were.
    pub(super) fn prepend(&mut self, peer: [u8; 16], older: Vec<Message>, earlier: usize)
    where
        B: InputBuffer,
    {
        let Some(c) = self.conversations.iter().position(|c| c.peer == peer) else {
            return;
        };
        let n = older.len();
        let open = c == self.selected;
        let rows_before = match open {
            true => self.laid().rows.len(),
            false => 0,
        };
        let conversation = &mut self.conversations[c];
        conversation.earlier = earlier;
        conversation.messages.splice(0..0, older);
        if let Some(rule) = conversation.new_rule.as_mut() {
            rule.at += n;
        }
        for note in &mut conversation.notes {
            note.before += n;
        }
        for (sc, sm) in self.sending.iter_mut() {
            if *sc == c {
                *sm += n;
            }
        }
        if let Some((dc, dm)) = self.detail.as_mut() {
            if *dc == c {
                *dm += n;
            }
        }
        if open {
            self.message_cursor += n;
            self.selection = None;
            self.press = None;
            let grown = self.laid().rows.len().saturating_sub(rows_before);
            if let Some(top) = self.scroll_top.as_mut() {
                *top += grown;
            }
        }
    }

    /// The store's answer to [`StoreRead::Earlier`]: the page goes above
    /// the conversation, and a search result waiting for it opens.
    pub(super) fn on_earlier(&mut self, peer: [u8; 16], older: Vec<Message>, earlier: usize)
    where
        B: InputBuffer,
    {
        self.prepend(peer, older, earlier);
        let Some((want_peer, key)) = self.reveal_pending else {
            return;
        };
        if want_peer != peer {
            return;
        }
        self.reveal_pending = None;
        let Some(c) = self.conversations.iter().position(|c| c.peer == peer) else {
            return;
        };
        let Some(m) = self.conversations[c]
            .messages
            .iter()
            .position(|message| message.activity == key)
        else {
            return;
        };
        if c != self.selected {
            self.select(c);
        }
        self.focus = super::Pane::Messages;
        super::reveal(self, m);
    }

    /// The open conversation's next page from the store, when the cursor
    /// reached the top of what is loaded and the store holds more.
    pub(super) fn earlier_wanted(&self) -> Vec<UiEffect> {
        let Some(conversation) = self.conversations.get(self.selected) else {
            return Vec::new();
        };
        let at_top = self.message_cursor == 0 || self.scroll_top == Some(0);
        match (conversation.earlier, conversation.messages.first()) {
            (0, _) | (_, None) => Vec::new(),
            (_, Some(first)) if at_top => vec![UiEffect::Store(StoreRead::Earlier {
                peer: conversation.peer,
                before: first.activity,
                through: None,
            })],
            _ => Vec::new(),
        }
    }

    /// `d`, or the palette's `delete message`: ask first.
    pub(super) fn ask_delete_message(&mut self) {
        let Some(conversation) = self.conversations.get(self.selected) else {
            self.toast(ToastKind::Info, "no message here".to_string());
            return;
        };
        let Some(message) = conversation.messages.get(self.message_cursor) else {
            self.toast(ToastKind::Info, "no message here".to_string());
            return;
        };
        if self.sending.contains(&(self.selected, self.message_cursor)) {
            self.toast(
                ToastKind::Info,
                "still being sent: delete it once it settles".to_string(),
            );
            return;
        }
        let confirm = Confirm::Message {
            peer: conversation.peer,
            key: message.activity,
        };
        let place = self.deleted_from();
        self.arm(
            confirm,
            format!("delete this message from {place}? y deletes, any other key keeps it"),
        );
    }

    /// The palette's `delete conversation`: ask first.
    pub(super) fn ask_delete_conversation(&mut self) {
        let Some(conversation) = self.conversations.get(self.selected) else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return;
        };
        if self.sending.iter().any(|&(c, _)| c == self.selected) {
            self.toast(
                ToastKind::Info,
                "a message in it is still being sent: delete it once that settles".to_string(),
            );
            return;
        }
        let label = self.names.label(&conversation.peer);
        let confirm = Confirm::Conversation {
            peer: conversation.peer,
        };
        self.arm(
            confirm,
            format!(
                "delete the conversation with {label} from {}? y deletes, any other key keeps it",
                self.deleted_from()
            ),
        );
    }

    /// Where a delete reaches: on a client of a home node, every device of
    /// the home, since the delete is marked for the others.
    fn deleted_from(&self) -> &'static str {
        match self.home_client {
            true => "every device of this home",
            false => "this device",
        }
    }

    fn arm(&mut self, confirm: Confirm, text: String) {
        self.confirm = Some((
            confirm,
            Toast {
                kind: ToastKind::Error,
                text,
                shown_at: self.tick,
            },
        ));
    }

    /// The key after a question: `y` does what was asked, any other keeps
    /// everything as it was.
    pub(super) fn answer(&mut self, yes: bool) {
        let Some((confirm, _)) = self.confirm.take() else {
            return;
        };
        if !yes {
            self.toast(ToastKind::Info, "kept".to_string());
            return;
        }
        match confirm {
            Confirm::Message { peer, key } => self.delete_message(peer, key),
            Confirm::Conversation { peer } => self.delete_conversation(peer, true),
        }
    }

    /// Delete one message from the device: the screen and the store. On a
    /// client of a home node, from the home's other devices too.
    fn delete_message(&mut self, peer: [u8; 16], key: u64) {
        let Some(id) = self.remove_message(peer, key) else {
            return;
        };
        self.forgotten.push(StoreWrite::ForgetMessage { key });
        let text = match (self.home_client, id) {
            (true, Some(id)) => {
                self.mark_out(MarkFlag::Deleted, vec![id]);
                "message deleted from every device of this home"
            }
            _ => "message deleted from this device",
        };
        self.toast(ToastKind::Info, text.to_string());
    }

    /// Delete the messages `keys` of `peer`'s conversation, asked already:
    /// as [`Self::delete_message`] each, one mark for the home's other
    /// devices and one line for all. One still being sent is kept, as `d`
    /// keeps it.
    pub(super) fn delete_messages(&mut self, peer: [u8; 16], keys: &[u64]) {
        let Some(c) = self.conversations.iter().position(|c| c.peer == peer) else {
            return;
        };
        let mut ids = Vec::new();
        let (mut deleted, mut kept) = (0usize, 0usize);
        for &key in keys {
            let at = self.conversations[c]
                .messages
                .iter()
                .position(|m| m.activity == key);
            let Some(m) = at else {
                continue;
            };
            if self.sending.contains(&(c, m)) {
                kept += 1;
                continue;
            }
            if let Some(id) = self.remove_message(peer, key) {
                self.forgotten.push(StoreWrite::ForgetMessage { key });
                ids.extend(id);
                deleted += 1;
            }
        }
        if deleted == 0 && kept == 0 {
            return;
        }
        let place = match self.home_client && !ids.is_empty() {
            true => "every device of this home",
            false => "this device",
        };
        if self.home_client && !ids.is_empty() {
            self.mark_out(MarkFlag::Deleted, ids);
        }
        let mut text = match deleted {
            1 => format!("message deleted from {place}"),
            n => format!("{n} messages deleted from {place}"),
        };
        if kept > 0 {
            text = format!("{text}; {kept} still being sent, kept");
        }
        self.toast(ToastKind::Info, text);
    }

    /// Another device of the home deleted the message `id`: gone here too,
    /// from the screen if it shows it and from the store either way.
    pub(super) fn forget_by_id(&mut self, id: &[u8; 32]) {
        let shown = self.conversations.iter().find_map(|c| {
            c.messages
                .iter()
                .find(|m| m.id == Some(*id))
                .map(|m| (c.peer, m.activity))
        });
        if let Some((peer, key)) = shown {
            self.remove_message(peer, key);
        }
        self.forgotten.push(StoreWrite::ForgetId { id: *id });
    }

    /// Another device of the home read the message `id`: every received
    /// message up to it in its conversation is read, as opening the
    /// conversation there read them.
    pub(super) fn read_through(&mut self, id: &[u8; 32]) {
        let Some(conversation) = self
            .conversations
            .iter_mut()
            .find(|c| c.messages.iter().any(|m| !m.outbound && m.id == Some(*id)))
        else {
            return;
        };
        let after = conversation
            .messages
            .iter()
            .rev()
            .take_while(|m| m.id != Some(*id))
            .filter(|m| !m.outbound)
            .count();
        if conversation.unread > after {
            conversation.unread = after;
            if after == 0 {
                conversation.new_rule = None;
            }
            self.changed_peers.insert(conversation.peer);
        }
    }

    /// Take one message off the screen, every index into its conversation
    /// moved with it: its LXMF id, `None` when it is not there, `Some(None)`
    /// for one that never had an id.
    fn remove_message(&mut self, peer: [u8; 16], key: u64) -> Option<Option<[u8; 32]>> {
        let c = self.conversations.iter().position(|c| c.peer == peer)?;
        let m = self.conversations[c]
            .messages
            .iter()
            .position(|message| message.activity == key)?;
        let conversation = &mut self.conversations[c];
        let id = conversation.messages.remove(m).id;
        let len = conversation.messages.len();
        if let Some(rule) = conversation.new_rule {
            conversation.new_rule = match rule.at {
                at if at > m => Some(NewRule { at: at - 1, ..rule }),
                at if at >= len => None,
                _ => Some(rule),
            };
        }
        for note in &mut conversation.notes {
            if note.before > m {
                note.before -= 1;
            }
        }
        for (sc, sm) in self.sending.iter_mut() {
            if *sc == c && *sm > m {
                *sm -= 1;
            }
        }
        self.detail = match self.detail {
            Some((dc, dm)) if dc == c && dm == m => None,
            Some((dc, dm)) if dc == c && dm > m => Some((dc, dm - 1)),
            other => other,
        };
        if c == self.selected {
            if self.message_cursor > m {
                self.message_cursor -= 1;
            }
            self.message_cursor = self.message_cursor.min(len.saturating_sub(1));
            self.selection = None;
            self.press = None;
        }
        self.changed.remove(&(peer, key));
        Some(id)
    }

    /// Delete a conversation and its messages from the device, and on a
    /// client of a home node from every device of the home when
    /// `everywhere`.
    pub(super) fn delete_conversation(&mut self, peer: [u8; 16], everywhere: bool) {
        let Some(c) = self.conversations.iter().position(|c| c.peer == peer) else {
            return;
        };
        let label = self.names.label(&peer);
        let removed = self.conversations.remove(c);
        let everywhere = everywhere && self.home_client;
        if everywhere {
            let ids = removed.messages.iter().filter_map(|m| m.id).collect();
            self.mark_out(MarkFlag::Deleted, ids);
        }
        for (sc, _) in self.sending.iter_mut() {
            if *sc > c {
                *sc -= 1;
            }
        }
        self.detail = match self.detail {
            Some((dc, _)) if dc == c => None,
            Some((dc, dm)) if dc > c => Some((dc - 1, dm)),
            other => other,
        };
        self.changed.retain(|(p, _)| *p != peer);
        self.changed_peers.remove(&peer);
        self.positions.remove(&peer);
        self.fixes_received.retain(|fix| fix.peer != peer);
        self.forgotten.push(StoreWrite::ForgetConversation { peer });
        if self.selected == c {
            let open = self.order().first().copied().unwrap_or(0);
            self.selected = open;
            self.scroll_top = None;
            self.selection = None;
            self.message_cursor = self
                .conversations
                .get(open)
                .map_or(0, |c| c.messages.len().saturating_sub(1));
        } else if self.selected > c && self.selected != super::NONE_OPEN {
            self.selected -= 1;
        }
        if let Some(search) = self.search.as_mut() {
            search.selected = 0;
            search.stored.clear();
        }
        let place = match everywhere {
            true => "every device of this home",
            false => "this device",
        };
        self.toast(
            ToastKind::Info,
            format!("conversation with {label} deleted from {place}"),
        );
    }

    /// The palette's `pin`: the open conversation goes above the rule, or
    /// back below it.
    /// On a client of a home node the pin reaches the home's other devices
    /// (1146).
    pub(super) fn toggle_pin(&mut self) -> Vec<UiEffect> {
        let Some(peer) = self.conversations.get(self.selected).map(|c| c.peer) else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return Vec::new();
        };
        let mut state = self.peer_state(&peer);
        state.pinned = !state.pinned;
        let effects = self.set_peer_state(peer, state);
        let label = self.names.label(&peer);
        let text = match state.pinned {
            true => format!("pinned {label}"),
            false => format!("unpinned {label}"),
        };
        self.toast(ToastKind::Info, text);
        effects
    }

    /// The store's search answer, kept while the field still asks it.
    pub(super) fn on_stored_hits(&mut self, query: &str, hits: Vec<StoredHit>)
    where
        B: InputBuffer,
    {
        if self.query() != Some(query) {
            return;
        }
        if let Some(search) = self.search.as_mut() {
            search.stored = hits;
        }
    }

    /// Whether some conversation holds more in the store than it shows: only
    /// then is the store asked when the query changes.
    pub(super) fn store_has_more(&self) -> bool {
        self.conversations.iter().any(|c| c.earlier > 0)
    }

    /// The first loaded key of every conversation that shows less than the
    /// store holds: the store's search looks below these, the screen's own
    /// matcher above them.
    pub fn loaded_from(&self) -> BTreeMap<[u8; 16], u64> {
        self.conversations
            .iter()
            .filter(|c| c.earlier > 0)
            .filter_map(|c| c.messages.first().map(|m| (c.peer, m.activity)))
            .collect()
    }

    /// Open a stored search result: its conversation loads down to it, then
    /// it opens at that message ([`Self::on_earlier`]).
    pub(super) fn open_stored(&mut self, hit: &StoredHit) -> Vec<UiEffect> {
        let Some(c) = self.conversations.iter().position(|c| c.peer == hit.peer) else {
            return Vec::new();
        };
        let Some(first) = self.conversations[c].messages.first() else {
            return Vec::new();
        };
        self.reveal_pending = Some((hit.peer, hit.key));
        vec![UiEffect::Store(StoreRead::Earlier {
            peer: hit.peer,
            before: first.activity,
            through: Some(hit.key),
        })]
    }
}
