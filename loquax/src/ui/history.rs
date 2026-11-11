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

use super::{Conversation, Message, NewRule, UiEffect, UiModel};
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
    /// Not the reader's word but kept beside it: the peer's latest message
    /// carried the `lev` read-receipt mark (1230), so the peer sends read
    /// receipts and wants them. The latest message decides; a receipt from
    /// the peer proves it too.
    pub receipts: bool,
}

/// How long typing must pause before a draft is written: a keystroke
/// moves the write on by this much (1219).
pub const DRAFT_DEBOUNCE_MS: u64 = 500;

/// The most read marks held for pages still to load (1490): older ones
/// are the store's to apply, which it already did.
pub const READ_PENDING_MOST: usize = 256;

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
    /// The group this conversation is, as the store kept it (1307 G2):
    /// listed with its members before any of its messages is loaded.
    pub group: Option<super::groups::GroupInfo>,
    /// The row as the store kept it (1490, schema 17): present when the
    /// start read hands summaries instead of messages, so the list
    /// shows without a body loaded.
    pub listing: Option<super::listing::Listing>,
}

/// What [`UiModel::take_touched`] hands a store that keeps rows, not
/// messages (1521b).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Touched {
    /// The conversations whose row changed, deleted ones left out.
    pub peers: alloc::collections::BTreeSet<[u8; 16]>,
    /// The conversations deleted, in the order they went.
    pub gone: Vec<[u8; 16]>,
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
    /// Keep the conversation's position share as it stands, none deleting
    /// it (1273): a share survives a restart.
    Share {
        peer: [u8; 16],
        share: Option<super::share::Share>,
    },
    /// Keep a group as the model knows it now (1307 G2).
    Group(super::groups::GroupInfo),
    /// Keep what the propagation node is still to be asked about, all of
    /// it in place of what was kept (1409).
    PnAsks(crate::pn_status::AskerState),
    /// Keep the conversation's row as the list shows it (1490, schema
    /// 17): written beside the fold that changed it, read back by the
    /// start's summary read.
    Listing {
        peer: [u8; 16],
        listing: super::listing::Listing,
    },
    /// Another device read through message `id` and the screen does not
    /// hold it (1490, the lazy start): the store applies the mark to
    /// its own rows, so the count a restart reads is the read one.
    ReadThrough { id: [u8; 32] },
    /// Keep the petnames this device owes the home node, all of them in
    /// place of what was kept (1635, schema 19): a restart hands them
    /// again.
    OwedPetnames(alloc::collections::BTreeMap<[u8; 16], Option<String>>),
    /// Keep the peer states this device owes the home node, all of them
    /// in place of what was kept (1640, schema 20).
    OwedPeerStates(alloc::collections::BTreeMap<[u8; 16], crate::home::wire::PeerState>),
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
        // The groups the restored messages speak of (1307).
        self.learn_groups();
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
    pub(super) fn restore_one(&mut self, stored: Restored) {
        if stored.messages.is_empty()
            && !stored.pinned
            && stored.flags == Flags::default()
            && stored.draft.is_empty()
            && stored.group.is_none()
            && stored.listing.is_none()
        {
            return;
        }
        // The group as the store kept it (1307 G2); what the loaded
        // messages say is learned over it, the newest epoch winning. Its
        // name too: a group whose invitation is beyond the loaded page, or
        // made here with no message yet, has only the store's (pass five).
        if let Some(group) = stored.group.clone() {
            if !group.name.is_empty() {
                self.names.remember(group.address, &group.name);
            }
            self.groups.insert(group.address, group);
        }
        let now = self.now;
        let index = self.conversation(stored.peer);
        let group = self.groups.contains_key(&stored.peer);
        let conversation = &mut self.conversations[index];
        conversation.listing = stored.listing;
        conversation.messages.set(stored.messages);
        // The store's arrival order, in its writers' (1505); a group's is
        // its home's acceptance, the order every member sees.
        if !group {
            let _ = conversation.sort_written();
        }
        conversation.pinned = stored.pinned;
        conversation.flags = stored.flags;
        conversation.earlier = stored.earlier;
        conversation.unread = stored.unread;
        conversation.draft = stored.draft;
        conversation.new_rule = conversation.rule_from_unread();
        conversation.activity = conversation.list_activity();
        let changed = &mut self.changed;
        conversation.messages.for_each_mut(|message| {
            if cut_off(message) {
                message.delivery = Some(DeliveryState::Queued);
                message.note(now, RESTARTED);
                changed.insert((stored.peer, message.activity));
            }
            message.leg = None;
        });
        // The summary and the change stamp start fresh over what was
        // restored (1490): the first fold after a start bumps nothing
        // and re-writes no row the store just handed out.
        conversation
            .summary
            .refresh(conversation.peer, &conversation.messages);
        conversation.stamp = super::Rev::at(conversation.row_fingerprint());
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
            let first = conversation.oldest_key().unwrap_or(first.activity);
            let (older, more) = below(stored.peer, first)?;
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
        // Every peer this batch writes a message or a conversation row
        // for owes a fresh listing in the same batch (1490): the store
        // nulls a conversation's row when a message touches it without
        // one, so an owed row must ride with its own writes, never a
        // fold later.
        let mut touched: alloc::collections::BTreeSet<[u8; 16]> =
            core::mem::take(&mut self.listing_owed);
        let mut changed = alloc::collections::BTreeSet::new();
        for owed in core::mem::take(&mut self.owed_changed) {
            match changed.is_empty() {
                true => changed = owed,
                false => changed.extend(owed),
            }
        }
        changed.extend(core::mem::take(&mut self.changed));
        for (peer, key) in changed {
            let found = self
                .conversations
                .iter()
                .find(|c| c.peer == peer)
                .and_then(|c| c.messages.iter().find(|m| m.activity == key));
            if let Some(message) = found {
                touched.insert(peer);
                self.ops.add(super::counters::Op::Copied, 1);
                writes.push(StoreWrite::Message {
                    peer,
                    message: message.clone(),
                });
            }
        }
        let mut peers = core::mem::take(&mut self.owed_peers);
        peers.extend(core::mem::take(&mut self.changed_peers));
        for peer in peers {
            if let Some(c) = self.conversations.iter().find(|c| c.peer == peer) {
                touched.insert(peer);
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
        writes.extend(
            self.take_share_writes()
                .into_iter()
                .map(|(peer, share)| StoreWrite::Share { peer, share }),
        );
        let mut groups = core::mem::take(&mut self.owed_groups);
        groups.extend(core::mem::take(&mut self.groups_changed));
        for address in groups {
            if let Some(group) = self.groups.get(&address) {
                writes.push(StoreWrite::Group(group.clone()));
            }
        }
        if let Some(state) = self.pn_asker.take_changed() {
            writes.push(StoreWrite::PnAsks(state));
        }
        if let Some(owed) = self.owed_petnames.take_changed() {
            writes.push(StoreWrite::OwedPetnames(owed));
        }
        if let Some(owed) = self.owed_peer_states.take_changed() {
            writes.push(StoreWrite::OwedPeerStates(owed));
        }
        // The rows the list shows, re-derived beside what changed them
        // (1490): a conversation with nothing loaded writes none, and
        // the store heals its own from a page then.
        for peer in touched {
            if let Some(listing) = self.listing_for(&peer) {
                writes.push(StoreWrite::Listing { peer, listing });
            }
        }
        writes.append(&mut self.forgotten);
        writes
    }

    /// What a fold changed, by conversation, for a shell whose store
    /// keeps its own entries and only a row per conversation (1521b, the
    /// M9): the queues [`Self::take_writes`] drains, drained alike, with
    /// no message copied and no listing built. The board wants the peers
    /// whose rows changed and the conversations deleted; [`Self::take_writes`]
    /// cloned every changed message to hand it that, and on a restore
    /// that is every message the store holds (1521b: the clones and their
    /// drop under the board's first-fit heap were 56 s of window 7's
    /// boot).
    pub fn take_touched(&mut self) -> Touched {
        let mut peers = core::mem::take(&mut self.listing_owed);
        for owed in core::mem::take(&mut self.owed_changed) {
            peers.extend(owed.into_iter().map(|(peer, _)| peer));
        }
        peers.extend(
            core::mem::take(&mut self.changed)
                .into_iter()
                .map(|(peer, _)| peer),
        );
        peers.append(&mut self.owed_peers);
        peers.append(&mut self.changed_peers);
        self.fixes_received.clear();
        let now = self.elapsed_ms;
        let quit = self.quit;
        self.drafts_due.retain(|_, &mut at| !(quit || at <= now));
        let _ = self.take_share_writes();
        self.owed_groups.clear();
        self.groups_changed.clear();
        let _ = self.pn_asker.take_changed();
        let mut gone = Vec::new();
        for write in core::mem::take(&mut self.forgotten) {
            if let StoreWrite::ForgetConversation { peer } = write {
                peers.remove(&peer);
                gone.push(peer);
            }
        }
        Touched { peers, gone }
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

    /// Put `message` into conversation `c` where its writer's time puts
    /// it (1505), and answer where; a group's copy goes last, its home's
    /// acceptance order being the group's. An arrival in order is pushed; a late
    /// one (written 09:00, taken from the mailbox at 14:00) is spliced in
    /// above the newer ones, and every index into the conversation moves
    /// with the messages it names: the `new` rule, the notes, the sends,
    /// the detail view, the cursor. A reader scrolled up in the terminal
    /// keeps the rows on screen where they were when it lands above
    /// them; the window's list holds its distance from the bottom and the
    /// M9 its anchor by key, which keep them without help.
    pub(crate) fn place(&mut self, c: usize, message: Message) -> usize
    where
        B: InputBuffer,
    {
        let Some(conversation) = self.conversations.get(c) else {
            return 0;
        };
        // A group's body runs in its home's acceptance order, the order
        // every member sees (`crate::home::group`), which is the order its
        // copies come in: a copy shows its writer's time, but goes last.
        let group = self.groups.contains_key(&conversation.peer)
            || leviculum_lxmf::lev::Lev::from_fields(&message.fields)
                .is_some_and(|lev| lev.group_copy.is_some());
        let conversation = &mut self.conversations[c];
        let at = match group {
            true => conversation.messages.len(),
            false => conversation.written_place(message.at.map(|at| at.unix_secs)),
        };
        if at == conversation.messages.len() {
            conversation.messages.push(message);
            return at;
        }
        let open = c == self.selected;
        // The message on the first row on screen, when the bubble lands
        // above it: the fold's end puts the row back on it.
        if let (true, Some(top), None) = (open, self.scroll_top, self.kept_top) {
            let laid = self.laid();
            let first = laid
                .spans
                .iter()
                .position(|s| s.start < s.end && s.end > top);
            if let Some(first) = first.filter(|&first| at <= first) {
                let block = laid.spans[first].block.0;
                self.kept_top = conversation_key(&self.conversations[c], first)
                    .map(|key| (key, top as isize - block as isize));
            }
        }
        let conversation = &mut self.conversations[c];
        conversation.messages.insert(at, message);
        conversation.summary.forget();
        self.shift_from(c, at);
        if open {
            if self.message_cursor >= at {
                self.message_cursor += 1;
            }
            self.selection = None;
            self.press = None;
        }
        at
    }

    /// Put the view's first row back on the message it was on before a
    /// late bubble went in above it (1505, [`Self::place`]): the rows on
    /// screen stay where they were, whatever the fold added above them.
    pub(super) fn keep_top(&mut self)
    where
        B: InputBuffer,
    {
        let Some((key, offset)) = self.kept_top.take() else {
            return;
        };
        let Some(c) = self.conversations.get(self.selected) else {
            return;
        };
        if self.scroll_top.is_none() {
            return;
        }
        let Some(m) = c.messages.iter().position(|m| m.activity == key) else {
            return;
        };
        let laid = self.laid();
        let Some(span) = laid.spans.get(m) else {
            return;
        };
        let top = (span.block.0 as isize + offset).max(0) as usize;
        self.scroll_top = Some(top);
    }

    /// One message went in at `at` of conversation `c`: what named the
    /// messages from there on names them one further (1505). The cursor is
    /// the caller's, which knows whether the reader follows.
    fn shift_from(&mut self, c: usize, at: usize) {
        let conversation = &mut self.conversations[c];
        if let Some(rule) = conversation.new_rule.as_mut() {
            if rule.at >= at {
                rule.at += 1;
            }
        }
        for note in &mut conversation.notes {
            if note.before >= at {
                note.before += 1;
            }
        }
        for (sc, sm) in self.sending.iter_mut() {
            if *sc == c && *sm >= at {
                *sm += 1;
            }
        }
        if let Some((dc, dm)) = self.detail.as_mut() {
            if *dc == c && *dm >= at {
                *dm += 1;
            }
        }
    }

    /// Put conversation `c` in its writers' order (1505), a page the store
    /// handed out in arrival order, and move what names its messages with
    /// them. Nothing moves when it was in order.
    fn sort_written(&mut self, c: usize) {
        let group = self
            .conversations
            .get(c)
            .is_some_and(|conversation| self.groups.contains_key(&conversation.peer));
        if group {
            return;
        }
        let Some(new_of) = self
            .conversations
            .get_mut(c)
            .and_then(Conversation::sort_written)
        else {
            return;
        };
        let moved = |i: usize| new_of.get(i).copied().unwrap_or(i);
        let conversation = &mut self.conversations[c];
        if let Some(rule) = conversation.new_rule.as_mut() {
            rule.at = moved(rule.at);
        }
        for note in &mut conversation.notes {
            note.before = moved(note.before);
        }
        for (sc, sm) in self.sending.iter_mut() {
            if *sc == c {
                *sm = moved(*sm);
            }
        }
        if let Some((dc, dm)) = self.detail.as_mut() {
            if *dc == c {
                *dm = moved(*dm);
            }
        }
        if c == self.selected {
            self.message_cursor = moved(self.message_cursor);
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
        let first_fill = self.conversations[c].messages.is_empty();
        let rows_before = match open {
            true => self.laid().rows.len(),
            false => 0,
        };
        let conversation = &mut self.conversations[c];
        conversation.earlier = earlier;
        conversation.messages.prepend(older);
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
        // Read marks that came before their messages (1490): the page
        // that holds their conversation applies them now.
        if !self.read_pending.is_empty() {
            for id in core::mem::take(&mut self.read_pending) {
                let applies = self.conversations[c]
                    .with_id(&id, &self.ops)
                    .into_iter()
                    .any(|m| !self.conversations[c].messages[m].outbound);
                match applies {
                    true => self.read_through(&id),
                    false => self.read_pending.push(id),
                }
            }
        }
        if open && !first_fill {
            self.message_cursor += n;
        }
        // In its writers' order (1505): a page comes in arrival order, and
        // one the late arrivals below it were written among.
        self.sort_written(c);
        if first_fill {
            // The page a restart deferred (1490): the conversation lands
            // as a restore lands it, its rule where the unread begin
            // and a send the restart cut off back to `○` with its
            // ledger line, exactly as `restore` landed it.
            let now = self.now;
            let conversation = &mut self.conversations[c];
            let peer = conversation.peer;
            let changed = &mut self.changed;
            conversation.messages.for_each_mut(|message| {
                if cut_off(message) {
                    message.delivery = Some(DeliveryState::Queued);
                    message.note(now, RESTARTED);
                    changed.insert((peer, message.activity));
                }
                message.leg = None;
            });
            let conversation = &mut self.conversations[c];
            conversation.new_rule = conversation.rule_from_unread();
            if open {
                self.read_out(c);
                self.message_cursor = self.conversations[c].messages.len().saturating_sub(1);
                self.scroll_top = None;
                self.selection = None;
                self.press = None;
            }
            return;
        }
        if open {
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
            (0, _) => Vec::new(),
            // Nothing loaded yet (1490): the newest page.
            (_, None) => vec![UiEffect::Store(StoreRead::Earlier {
                peer: conversation.peer,
                before: u64::MAX,
                through: None,
            })],
            (_, Some(first)) if at_top => vec![UiEffect::Store(StoreRead::Earlier {
                peer: conversation.peer,
                before: conversation.oldest_key().unwrap_or(first.activity),
                through: None,
            })],
            _ => Vec::new(),
        }
    }

    /// The page above the open chat's first loaded bubble, when a
    /// frontend's viewport shows that bubble and the store holds more
    /// (1496). The phone scrolls in a list of its own, so its viewport is
    /// its only word that the reader reached the top of what is loaded;
    /// before, the phone's chat ended after one page.
    pub(super) fn earlier_shown(&self, peer: [u8; 16], keys: &[u64]) -> Vec<UiEffect> {
        let Some(conversation) = self.conversations.get(self.selected) else {
            return Vec::new();
        };
        if conversation.peer != peer || conversation.earlier == 0 {
            return Vec::new();
        }
        match conversation.messages.first() {
            Some(first) if keys.contains(&first.activity) => {
                vec![UiEffect::Store(StoreRead::Earlier {
                    peer,
                    before: conversation.oldest_key().unwrap_or(first.activity),
                    through: None,
                })]
            }
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
        let label = self.names.label(&conversation.peer, self.lang);
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
        self.saved_goes(peer, Some(key));
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
                self.saved_goes(peer, Some(key));
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

    /// The message `key` of `peer`'s conversation, or the whole
    /// conversation for no key, was deleted (1662): what this device saved
    /// of it goes too ([`super::UiEffect::ForgetSaved`]), and a transfer
    /// it had ends.
    pub(super) fn saved_goes(&mut self, peer: [u8; 16], key: Option<u64>) {
        match key {
            Some(key) => self.transfers.end(&peer, key),
            None => self.transfers.end_all(&peer),
        }
        self.saved_gone.push((peer, key));
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
            self.saved_goes(peer, Some(key));
        }
        self.forgotten.push(StoreWrite::ForgetId { id: *id });
    }

    /// Another device of the home read the message `id`: every received
    /// message up to it in its conversation is read, as opening the
    /// conversation there read them.
    pub(super) fn read_through(&mut self, id: &[u8; 32]) {
        self.read_marks(core::slice::from_ref(id));
    }

    /// Another device of the home read through each of `ids`, in turn
    /// ([`UiModel::read_through`]): the id index brought up once, each id
    /// then one lookup (1600). Before, each id asked every conversation
    /// whether it held it, 302 of them on the M9 at 38 ms a mark.
    pub(super) fn read_marks(&mut self, ids: &[[u8; 32]]) {
        self.ids.reconcile(&mut self.conversations);
        for id in ids {
            self.read_through_indexed(id);
        }
    }

    /// The model's id index, for a test to hold against a rebuild.
    #[cfg(test)]
    pub(super) fn id_index(&mut self) -> &super::id_index::IdIndex {
        self.ids.reconcile(&mut self.conversations);
        &self.ids
    }

    /// [`UiModel::read_through`] with the id index up to the messages:
    /// what changes here is a count and the kept ids, never a message, so
    /// the index stays up for the next id of the same mark. The index
    /// names the conversations; each is checked against its own messages,
    /// so the answer is the walk's, first conversation first.
    fn read_through_indexed(&mut self, id: &[u8; 32]) {
        let ops = &self.ops;
        let conversations = &mut self.conversations;
        let found = self.ids.holding(id).find(|&c| {
            ops.add(super::counters::Op::Probed, 1);
            conversations.get_mut(c).is_some_and(|c| {
                c.with_id(id, ops)
                    .into_iter()
                    .any(|m| !c.messages[m].outbound)
            })
        });
        let Some(conversation) = found.and_then(|c| self.conversations.get_mut(c)) else {
            // The screen does not hold the message (1490, the lazy
            // start): the store applies the mark to its own rows, and
            // the page that loads it here replays it.
            self.forgotten.push(StoreWrite::ReadThrough { id: *id });
            // A conversation listed from its kept row (1521) knows its
            // newest unread ids: the badge clears here too.
            self.read_through_unloaded(id);
            if self.read_pending.len() < READ_PENDING_MOST {
                self.read_pending.push(*id);
            }
            return;
        };
        // Received after it by arrival (1505): the body's writers' order
        // may put a late one above it, and the mark read it too.
        let through = conversation
            .messages
            .iter()
            .filter(|m| m.id == Some(*id))
            .map(|m| m.activity)
            .max()
            .unwrap_or(0);
        let after = conversation
            .messages
            .iter()
            .filter(|m| !m.outbound && m.activity > through)
            .count();
        if conversation.unread > after {
            conversation.unread = after;
            if after == 0 {
                conversation.new_rule = None;
            }
            self.changed_peers.insert(conversation.peer);
        }
    }

    /// A read mark through `id` on a conversation none of whose messages
    /// is loaded (1521): its kept unread ids say how many came after.
    fn read_through_unloaded(&mut self, id: &[u8; 32]) {
        let (ops, conversations) = (&self.ops, &self.conversations);
        let found = self.ids.keeping(id).find(|&c| {
            ops.add(super::counters::Op::Probed, 1);
            conversations
                .get(c)
                .is_some_and(|c| c.messages.is_empty() && c.unread_ids.contains(id))
        });
        let Some(conversation) = found.and_then(|c| self.conversations.get_mut(c)) else {
            return;
        };
        let at = conversation
            .unread_ids
            .iter()
            .position(|kept| kept == id)
            .unwrap_or(0);
        let after = conversation.unread_ids.len() - at - 1;
        if conversation.unread <= after {
            return;
        }
        conversation.unread = after;
        conversation.unread_ids.drop_through(at);
        if let Some(listing) = conversation.listing.as_mut() {
            listing.unread_rows = listing.unread_rows.min(after as u32);
        }
        if after == 0 {
            conversation.new_rule = None;
        }
        self.changed_peers.insert(conversation.peer);
    }

    /// Take one message off the screen, every index into its conversation
    /// moved with it: its LXMF id, `None` when it is not there, `Some(None)`
    /// for one that never had an id.
    pub(super) fn remove_message(&mut self, peer: [u8; 16], key: u64) -> Option<Option<[u8; 32]>> {
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
        for owed in &mut self.owed_changed {
            owed.remove(&(peer, key));
        }
        Some(id)
    }

    /// Delete a conversation and its messages from the device, and on a
    /// client of a home node from every device of the home when
    /// `everywhere`.
    pub(super) fn delete_conversation(&mut self, peer: [u8; 16], everywhere: bool) {
        let label = self.names.label(&peer, self.lang);
        let Some(removed) = self.remove_conversation(peer) else {
            return;
        };
        self.saved_goes(peer, None);
        let everywhere = everywhere && self.home_client;
        if everywhere {
            let ids = removed.messages.iter().filter_map(|m| m.id).collect();
            self.mark_out(MarkFlag::Deleted, ids);
        }
        self.forgotten.push(StoreWrite::ForgetConversation { peer });
        let place = match everywhere {
            true => "every device of this home",
            false => "this device",
        };
        self.toast(
            ToastKind::Info,
            format!("conversation with {label} deleted from {place}"),
        );
    }

    /// Take conversation `peer` out of the model, every index into the
    /// conversations moved past it: what a delete does on the screen,
    /// without its store write, its mark or its toast.
    pub(super) fn remove_conversation(&mut self, peer: [u8; 16]) -> Option<Conversation> {
        let c = self.conversations.iter().position(|c| c.peer == peer)?;
        let removed = self.conversations.remove(c);
        self.names.part(&peer);
        for (sc, _) in self.sending.iter_mut() {
            if *sc > c && *sc != super::receipts::RECEIPT_SEND && *sc != super::groups::STATE_SEND {
                *sc -= 1;
            }
        }
        self.detail = match self.detail {
            Some((dc, _)) if dc == c => None,
            Some((dc, dm)) if dc > c => Some((dc - 1, dm)),
            other => other,
        };
        self.changed.retain(|(p, _)| *p != peer);
        for owed in &mut self.owed_changed {
            owed.retain(|(p, _)| *p != peer);
        }
        self.changed_peers.remove(&peer);
        self.owed_peers.remove(&peer);
        self.positions.remove(&peer);
        self.tracks.remove(&peer);
        self.fixes_received.retain(|fix| fix.peer != peer);
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
        Some(removed)
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
        let label = self.names.label(&peer, self.lang);
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
            .filter_map(|c| c.oldest_key().map(|key| (c.peer, key)))
            .collect()
    }

    /// Open a stored search result: its conversation loads down to it, then
    /// it opens at that message ([`Self::on_earlier`]).
    pub(super) fn open_stored(&mut self, hit: &StoredHit) -> Vec<UiEffect> {
        let Some(c) = self.conversations.iter().position(|c| c.peer == hit.peer) else {
            return Vec::new();
        };
        let Some(first) = self.conversations[c].oldest_key() else {
            return Vec::new();
        };
        self.reveal_pending = Some((hit.peer, hit.key));
        vec![UiEffect::Store(StoreRead::Earlier {
            peer: hit.peer,
            before: first,
            through: Some(hit.key),
        })]
    }
}

/// The store key of message `m` of `conversation`.
fn conversation_key(conversation: &Conversation, m: usize) -> Option<u64> {
    conversation.messages.get(m).map(|message| message.activity)
}
