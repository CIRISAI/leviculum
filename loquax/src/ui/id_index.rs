//! Which conversation holds a message id, for the whole model (1600).
//!
//! A read mark from another device of the home names up to 64 ids, and a
//! board's pull folds 39 of them at its tail. Each id asked every
//! conversation in turn whether it held it, and then every unloaded one
//! whether its kept unread ids did: on the M9, 302 conversations made a
//! mark 38 ms to fold against 4.9 ms for a message (1597, window 21).
//! [`IdIndex`] answers in one lookup.
//!
//! The index is exact, not a hint: it equals what [`IdIndex::rebuild`]
//! reads off the conversations, which a test checks. That holds because
//! nothing changes a conversation's messages or its kept unread ids except
//! through the two types here: [`Messages`] owns the message vector and
//! [`KeptIds`] the kept ids, each with a private field, and every change
//! they make that adds, drops or renames an id is written to a small
//! journal beside it. [`IdIndex::reconcile`] reads the journals (one
//! pass over the conversations, never over their messages) and applies
//! them. What a journal cannot say, it says by refusing: a conversation
//! cloned, replaced, moved or deleted carries no mark of the place the
//! index knew it at, and the index reads everything anew.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::Deref;

use super::{Conversation, Message};

/// Changes a journal holds before it gives up and asks for a rebuild: a
/// few arrivals in one conversation between two marks, kept small because
/// every conversation may hold one (302 on the M9).
const JOURNAL_MOST: usize = 32;

/// The index's bookkeeping in one seam: where the index last read this
/// conversation, and what changed since.
#[derive(Debug, Default)]
struct Track {
    /// The conversation's peer and place when the index read it; `None`
    /// until then, and in a clone.
    at: Option<([u8; 16], u32)>,
    /// Ids added (`true`) and dropped (`false`) since.
    journal: Vec<([u8; 32], bool)>,
    /// More changed than the journal holds.
    overflow: bool,
}

impl Track {
    fn note(&mut self, id: Option<[u8; 32]>, added: bool) {
        let (Some(id), Some(_)) = (id, self.at) else {
            return;
        };
        if self.overflow {
            return;
        }
        if self.journal.len() == JOURNAL_MOST {
            self.overflow = true;
            self.journal = Vec::new();
            return;
        }
        self.journal.push((id, added));
    }
}

/// A conversation's messages, oldest first: read as a slice, changed only
/// through the methods here, so the model's [`IdIndex`] hears of every id
/// that comes, goes or is renamed.
#[derive(Debug, Default)]
pub struct Messages {
    list: Vec<Message>,
    track: Track,
}

impl Clone for Messages {
    /// The messages; a clone is a conversation the index has not read.
    fn clone(&self) -> Self {
        Self {
            list: self.list.clone(),
            track: Track::default(),
        }
    }
}

impl PartialEq for Messages {
    fn eq(&self, other: &Self) -> bool {
        self.list == other.list
    }
}

impl Eq for Messages {}

impl Deref for Messages {
    type Target = [Message];

    fn deref(&self) -> &[Message] {
        &self.list
    }
}

impl From<Vec<Message>> for Messages {
    fn from(list: Vec<Message>) -> Self {
        Self {
            list,
            track: Track::default(),
        }
    }
}

impl<'a> IntoIterator for &'a Messages {
    type Item = &'a Message;
    type IntoIter = core::slice::Iter<'a, Message>;

    fn into_iter(self) -> Self::IntoIter {
        self.list.iter()
    }
}

impl Messages {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, message: Message) {
        self.track.note(message.id, true);
        self.list.push(message);
    }

    pub fn insert(&mut self, at: usize, message: Message) {
        self.track.note(message.id, true);
        self.list.insert(at, message);
    }

    pub fn remove(&mut self, at: usize) -> Message {
        let message = self.list.remove(at);
        self.track.note(message.id, false);
        message
    }

    /// Keep the messages `keep` says yes to.
    pub fn retain(&mut self, mut keep: impl FnMut(&Message) -> bool) {
        let track = &mut self.track;
        self.list.retain(|message| {
            let kept = keep(message);
            if !kept {
                track.note(message.id, false);
            }
            kept
        });
    }

    /// Put `added` in front of the messages: an older page.
    pub fn prepend(&mut self, added: Vec<Message>) {
        for message in &added {
            self.track.note(message.id, true);
        }
        self.list.splice(0..0, added);
    }

    /// Put `added` after the messages.
    pub fn extend(&mut self, added: impl IntoIterator<Item = Message>) {
        for message in added {
            self.push(message);
        }
    }

    /// Hold `list` in place of the messages held.
    pub fn set(&mut self, list: Vec<Message>) {
        let _ = self.take();
        for message in &list {
            self.track.note(message.id, true);
        }
        self.list = list;
    }

    /// Every message gone.
    pub fn clear(&mut self) {
        let _ = self.take();
    }

    pub fn as_slice(&self) -> &[Message] {
        &self.list
    }

    /// The messages, the seam let go of.
    pub fn into_vec(self) -> Vec<Message> {
        self.list
    }

    /// Every message gone, handed back.
    pub fn take(&mut self) -> Vec<Message> {
        for message in &self.list {
            self.track.note(message.id, false);
        }
        core::mem::take(&mut self.list)
    }

    /// The messages in another order: `order` names each new place's old
    /// index. The ids stay where they are, so the index hears nothing.
    pub fn reorder(&mut self, order: &[usize]) {
        let mut old: Vec<Option<Message>> = core::mem::take(&mut self.list)
            .into_iter()
            .map(Some)
            .collect();
        self.list = order.iter().filter_map(|&i| old[i].take()).collect();
    }

    /// One message to change in place; a change of its id reaches the
    /// index when the handle goes.
    pub fn get_mut(&mut self, at: usize) -> Option<MessageMut<'_>> {
        let message = self.list.get_mut(at)?;
        Some(MessageMut {
            id: message.id,
            message,
            track: &mut self.track,
        })
    }

    /// The newest message to change in place.
    pub fn last_mut(&mut self) -> Option<MessageMut<'_>> {
        let last = self.list.len().checked_sub(1)?;
        self.get_mut(last)
    }

    /// The first message `found` says yes to, to change in place.
    pub fn find_mut(&mut self, found: impl FnMut(&Message) -> bool) -> Option<MessageMut<'_>> {
        let at = self.list.iter().position(found)?;
        self.get_mut(at)
    }

    /// Change every message in place, ids included.
    pub fn for_each_mut(&mut self, mut change: impl FnMut(&mut Message)) {
        self.for_each_indexed_mut(|_, message| change(message));
    }

    /// Change every message in place, each with its index.
    pub fn for_each_indexed_mut(&mut self, mut change: impl FnMut(usize, &mut Message)) {
        for (at, message) in self.list.iter_mut().enumerate() {
            let id = message.id;
            change(at, message);
            if message.id != id {
                self.track.note(id, false);
                self.track.note(message.id, true);
            }
        }
    }
}

/// One message borrowed to change: whatever it is, and its id's change
/// written to the index's journal when it is let go.
pub struct MessageMut<'a> {
    message: &'a mut Message,
    id: Option<[u8; 32]>,
    track: &'a mut Track,
}

impl Deref for MessageMut<'_> {
    type Target = Message;

    fn deref(&self) -> &Message {
        self.message
    }
}

impl core::ops::DerefMut for MessageMut<'_> {
    fn deref_mut(&mut self) -> &mut Message {
        self.message
    }
}

impl Drop for MessageMut<'_> {
    fn drop(&mut self) {
        if self.message.id != self.id {
            self.track.note(self.id, false);
            self.track.note(self.message.id, true);
        }
    }
}

/// A conversation's newest unread received ids while none of its messages
/// is loaded (1521): changed only through here, as [`Messages`] is.
#[derive(Debug, Default)]
pub struct KeptIds {
    ids: Vec<[u8; 32]>,
    track: Track,
}

impl Clone for KeptIds {
    fn clone(&self) -> Self {
        Self {
            ids: self.ids.clone(),
            track: Track::default(),
        }
    }
}

impl PartialEq for KeptIds {
    fn eq(&self, other: &Self) -> bool {
        self.ids == other.ids
    }
}

impl Eq for KeptIds {}

impl Deref for KeptIds {
    type Target = [[u8; 32]];

    fn deref(&self) -> &[[u8; 32]] {
        &self.ids
    }
}

impl From<Vec<[u8; 32]>> for KeptIds {
    fn from(ids: Vec<[u8; 32]>) -> Self {
        Self {
            ids,
            track: Track::default(),
        }
    }
}

impl KeptIds {
    /// Hold `ids` in place of what was held.
    pub fn set(&mut self, ids: Vec<[u8; 32]>) {
        for &id in &self.ids {
            self.track.note(Some(id), false);
        }
        for &id in &ids {
            self.track.note(Some(id), true);
        }
        self.ids = ids;
    }

    /// Every id gone, handed back.
    pub fn take(&mut self) -> Vec<[u8; 32]> {
        for &id in &self.ids {
            self.track.note(Some(id), false);
        }
        core::mem::take(&mut self.ids)
    }

    /// Drop the ids up to `at`, that one included.
    pub fn drop_through(&mut self, at: usize) {
        for &id in &self.ids[..=at] {
            self.track.note(Some(id), false);
        }
        self.ids.drain(..=at);
    }
}

/// Which conversations hold an id: among their messages, and among the
/// kept unread ids of the ones not loaded.
#[derive(Clone, Debug, Default)]
pub struct IdIndex {
    /// (id, conversation's place) to how many of its messages carry it.
    held: BTreeMap<Key, u32>,
    /// (id, conversation's place) to how often its kept ids name it.
    kept: BTreeMap<Key, u32>,
    /// How many conversations it has read.
    read: usize,
    /// How often it read everything anew: what a test asks to know the
    /// journals carried a change, not a rebuild.
    rebuilds: u32,
}

impl PartialEq for IdIndex {
    /// The same ids in the same places; how it got there aside.
    fn eq(&self, other: &Self) -> bool {
        (&self.held, &self.kept, self.read) == (&other.held, &other.kept, other.read)
    }
}

impl Eq for IdIndex {}

/// An id's first eight bytes, the id, and the conversation's place: the
/// first word decides nearly every comparison in one instruction, where
/// the id alone was a 32-byte compare per step of a lookup.
type Key = (u64, [u8; 32], u32);

fn key(id: [u8; 32], place: u32) -> Key {
    let mut head = [0; 8];
    head.copy_from_slice(&id[..8]);
    (u64::from_be_bytes(head), id, place)
}

fn count(map: &mut BTreeMap<Key, u32>, key: Key, added: bool) {
    if added {
        *map.entry(key).or_default() += 1;
    } else if let Some(n) = map.get_mut(&key) {
        *n -= 1;
        if *n == 0 {
            map.remove(&key);
        }
    }
}

fn at<'a>(map: &'a BTreeMap<Key, u32>, id: &[u8; 32]) -> impl Iterator<Item = usize> + 'a {
    map.range(key(*id, 0)..=key(*id, u32::MAX))
        .map(|(&(_, _, place), _)| place as usize)
}

/// Counted keys from sorted ones: each run of equal keys one entry.
fn counted(mut keys: Vec<Key>) -> BTreeMap<Key, u32> {
    keys.sort_unstable();
    let mut runs: Vec<(Key, u32)> = Vec::with_capacity(keys.len());
    for key in keys {
        match runs.last_mut() {
            Some((last, n)) if *last == key => *n += 1,
            _ => runs.push((key, 1)),
        }
    }
    // Sorted input: the map is built in one pass, not key by key.
    runs.into_iter().collect()
}

impl IdIndex {
    /// The index read off `conversations` from nothing.
    pub fn rebuild(conversations: &mut [Conversation]) -> Self {
        let mut held = Vec::new();
        let mut kept = Vec::new();
        for (place, conversation) in conversations.iter_mut().enumerate() {
            let place32 = u32::try_from(place).unwrap_or(u32::MAX);
            held.extend(
                conversation
                    .messages
                    .iter()
                    .filter_map(|m| m.id)
                    .map(|id| key(id, place32)),
            );
            kept.extend(conversation.unread_ids.iter().map(|&id| key(id, place32)));
            Self::mark_read(place32, conversation);
        }
        Self {
            held: counted(held),
            kept: counted(kept),
            read: conversations.len(),
            rebuilds: 0,
        }
    }

    /// The conversation at `place` is in the index as it stands.
    fn mark_read(place: u32, conversation: &mut Conversation) {
        let at = Some((conversation.peer, place));
        for track in [
            &mut conversation.messages.track,
            &mut conversation.unread_ids.track,
        ] {
            *track = Track {
                at,
                ..Track::default()
            };
        }
    }

    fn read_in(&mut self, place: usize, conversation: &mut Conversation) {
        let place32 = u32::try_from(place).unwrap_or(u32::MAX);
        for id in conversation.messages.iter().filter_map(|m| m.id) {
            count(&mut self.held, key(id, place32), true);
        }
        for &id in conversation.unread_ids.iter() {
            count(&mut self.kept, key(id, place32), true);
        }
        Self::mark_read(place32, conversation);
        self.read += 1;
    }

    /// Bring the index up to `conversations`: their journals applied, a
    /// conversation new to it read in, everything read anew when one it
    /// knew is gone, moved or replaced. One pass over the conversations,
    /// none over their messages unless it reads them in.
    pub fn reconcile(&mut self, conversations: &mut [Conversation]) {
        let mut known = 0;
        let mut fresh = Vec::new();
        let mut sound = true;
        for (place, conversation) in conversations.iter_mut().enumerate() {
            let place32 = u32::try_from(place).unwrap_or(u32::MAX);
            let at = Some((conversation.peer, place32));
            let (messages, kept) = (
                &mut conversation.messages.track,
                &mut conversation.unread_ids.track,
            );
            match (messages.at, kept.at) {
                (None, None) => fresh.push(place),
                (m, k) if m == at && k == at && !messages.overflow && !kept.overflow => {
                    known += 1;
                    // Applied before the pass knows it is sound: an
                    // unsound pass reads everything anew, these with it.
                    for (id, added) in core::mem::take(&mut messages.journal) {
                        count(&mut self.held, key(id, place32), added);
                    }
                    for (id, added) in core::mem::take(&mut kept.journal) {
                        count(&mut self.kept, key(id, place32), added);
                    }
                }
                _ => {
                    sound = false;
                    break;
                }
            }
        }
        if !sound || known != self.read {
            let rebuilds = self.rebuilds + 1;
            *self = Self::rebuild(conversations);
            self.rebuilds = rebuilds;
            return;
        }
        // Most of them new (the first mark): read in at once.
        if fresh.len() > known {
            let rebuilds = self.rebuilds;
            *self = Self::rebuild(conversations);
            self.rebuilds = rebuilds;
            return;
        }
        for place in fresh {
            self.read_in(place, &mut conversations[place]);
        }
    }

    /// The places of the conversations whose messages carry `id`, first
    /// place first.
    pub fn holding(&self, id: &[u8; 32]) -> impl Iterator<Item = usize> + '_ {
        at(&self.held, id)
    }

    /// How often [`IdIndex::reconcile`] read everything anew.
    pub fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    /// The places of the conversations whose kept unread ids name `id`.
    pub fn keeping(&self, id: &[u8; 32]) -> impl Iterator<Item = usize> + '_ {
        at(&self.kept, id)
    }
}
