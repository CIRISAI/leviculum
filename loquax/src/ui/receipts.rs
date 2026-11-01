//! Read receipts, slice one (1230): the wire and the Read state.
//!
//! `docs/src/concepts/loquax-read-receipts.md` is the design. What this
//! slice holds:
//!
//! - Every message the model sends carries the `lev` namespace's capability
//!   mark ([`leviculum_lxmf::lev::Lev::mark`]): "I send read receipts and
//!   want them". The mark rides in the custom field pair, which every
//!   client checked ignores.
//! - Every message received says whether its sender carries the mark, and
//!   the latest one decides the conversation's
//!   [`super::history::Flags::receipts`]: a peer that turned receipts off,
//!   or a client without them, stops being sent any. A receipt proves
//!   support too, whatever the message before it said.
//! - A receipt names our messages by their LXMF id; each one in the
//!   receipt's sender's conversation becomes [`DeliveryState::Read`], at the
//!   reader's time kept within when it was sent and when the receipt came.
//!   Read is monotone: a second receipt and a late proof leave it. An id the
//!   conversation does not hold as ours to that peer is dropped with a log
//!   line: nobody confirms a message that was not sent to them.
//! - A receipt alone, an empty message with nothing but the namespace, is a
//!   state and not a message, as a position report is (1209): no bubble, no
//!   unread, no toast, no move in the list, no conversation made for it.
//!
//! Slice two (1231): the read event, the batch and the piggyback.
//!
//! - The platform says only facts: which bubbles of which chat are on
//!   screen ([`super::UiEvent::Viewport`], at least half of each) and
//!   whether the app is in front with the screen on
//!   ([`super::UiEvent::Front`]). A terminal that never says its focus
//!   counts as in front once the reader pressed a key or the mouse in the
//!   open chat. The core decides the rest.
//! - A received message is seen when its bubble stayed on screen in the
//!   open chat, in front, for [`SEEN_MS`] without a break. It must be the
//!   peer's, not a reaction nor a position report, carry an id, and its
//!   peer must take receipts ([`super::history::Flags::receipts`]), not be
//!   blocked, and the switch ([`Reading::on`]) be on. Seen is kept in the
//!   message's ledger ([`SEEN`]), so a restart sends no receipt twice.
//! - What is seen gathers into one receipt a peer, with the reader's clock
//!   at the first one seen. It rides on the next message to that peer
//!   (the same LXMF message, the reply's own fields beside it); else it
//!   goes as a message of its own, with no text and no bubble,
//!   [`BATCH_MS`] after the last bubble seen or at once when the chat is
//!   left or the app goes to the back. A draft in the chat holds it up to
//!   [`DRAFT_WAIT_MS`] after the last bubble seen, for the reply. The
//!   runner sends it as every message, direct first and then by the
//!   mailbox, and nothing on screen follows it.
//! - On a client of a home node the device holds no identity, so it sends
//!   none yet: the home node signs them in a later slice (R5).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE};
use leviculum_lxmf::lev::{Lev, Receipt};
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;

use super::{to_hex, Trust, UiEffect, UiModel};

/// How long a bubble stays on screen, in front, before it is seen.
pub const SEEN_MS: u64 = 1_000;

/// How long a receipt waits after the last bubble seen for a message to
/// ride on.
pub const BATCH_MS: u64 = 5_000;

/// How long it waits instead while the chat holds a draft: the reply is
/// being written.
pub const DRAFT_WAIT_MS: u64 = 60_000;

/// The ledger line of a received message once it was seen, the word the
/// next start reads to send no second receipt.
pub const SEEN: &str = "seen";

/// The slot of [`super::UiModel::sending`] a receipt of its own takes: it
/// runs in the shell's queue like every send, and no bubble follows it.
pub const RECEIPT_SEND: usize = usize::MAX;

/// What the model knows of the reader's eyes, and the receipts owed.
#[derive(Clone, Debug)]
pub struct Reading {
    /// The switch (`read_receipts` in the config, the settings of 1233):
    /// off, no mark goes out and no receipt.
    pub on: bool,
    /// The platform's word on the front: `Some(true)` in front with the
    /// screen on, `Some(false)` behind; `None` while it said nothing.
    pub front: Option<bool>,
    /// The chat the reader pressed a key or the mouse in since it opened:
    /// a terminal without focus reports is in front for it.
    touched: Option<[u8; 16]>,
    /// The chat whose bubbles are on screen, and their keys.
    viewport: Option<([u8; 16], Vec<u64>)>,
    /// The bubbles counting now, by key: since when, on `elapsed_ms`.
    since: BTreeMap<u64, u64>,
    /// The receipt owed each peer.
    owed: BTreeMap<[u8; 16], Owed>,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            on: true,
            front: None,
            touched: None,
            viewport: None,
            since: BTreeMap::new(),
            owed: BTreeMap::new(),
        }
    }
}

impl Reading {
    /// The viewport last said, for a frontend that says it on change.
    pub fn viewport(&self) -> Option<&([u8; 16], Vec<u64>)> {
        self.viewport.as_ref()
    }

    /// The ids owed `peer` a receipt, oldest seen first.
    pub fn owed(&self, peer: &[u8; 16]) -> Option<&[[u8; 32]]> {
        self.owed.get(peer).map(|owed| owed.ids.as_slice())
    }
}

/// One receipt being gathered.
#[derive(Clone, Debug)]
struct Owed {
    /// The reader's clock when the first of them was seen.
    read_at: u32,
    ids: Vec<[u8; 32]>,
    /// When the last of them was seen, on `elapsed_ms`.
    last_ms: u64,
    /// The chat was left, or the app went to the back, since.
    left: bool,
}

/// Put the capability mark into every send among `effects` that does not
/// carry the namespace already (a receipt does, the mark in it).
pub(super) fn mark_sends(effects: &mut [UiEffect]) {
    for effect in effects {
        if let UiEffect::Send { fields, .. } = effect {
            if Lev::from_fields(fields).is_none() {
                Lev::mark().put_into(fields);
            }
        }
    }
}

/// Whether a received message is a receipt and nothing else: no text, no
/// field but the namespace's pair.
pub(super) fn alone(body: &[u8], fields: &[Field], lev: &Lev) -> bool {
    lev.receipt.is_some()
        && body.iter().all(u8::is_ascii_whitespace)
        && fields
            .iter()
            .all(|(key, _)| *key == FIELD_CUSTOM_TYPE || *key == FIELD_CUSTOM_DATA)
}

/// Whether a message's namespace says its sender sends and wants receipts.
fn supports(lev: Option<&Lev>) -> bool {
    lev.is_some_and(|lev| lev.supports_receipts() || lev.receipt.is_some())
}

impl<B> UiModel<B> {
    /// What a message from `source` with this namespace says: the peer's
    /// mark, and the receipt it carries. The log lines of the ids dropped.
    pub(super) fn take_lev(&mut self, source: [u8; 16], lev: Option<&Lev>) -> Vec<UiEffect> {
        let Some(index) = self.conversations.iter().position(|c| c.peer == source) else {
            return lev
                .and_then(|lev| lev.receipt.as_ref())
                .map(|receipt| dropped(source, &receipt.ids))
                .unwrap_or_default();
        };
        let conversation = &mut self.conversations[index];
        let supported = supports(lev);
        if conversation.flags.receipts != supported {
            conversation.flags.receipts = supported;
            self.changed_peers.insert(source);
        }
        match lev.and_then(|lev| lev.receipt.as_ref()) {
            Some(receipt) => self.apply_receipt(index, receipt),
            None => Vec::new(),
        }
    }

    fn apply_receipt(&mut self, index: usize, receipt: &Receipt) -> Vec<UiEffect> {
        let now = self.now;
        let conversation = &mut self.conversations[index];
        let peer = conversation.peer;
        let mut unknown = Vec::new();
        for id in &receipt.ids {
            let Some(message) = conversation
                .messages
                .iter_mut()
                .find(|m| m.outbound && m.id == Some(*id))
            else {
                unknown.push(*id);
                continue;
            };
            if matches!(message.delivery, Some(DeliveryState::Read { .. })) {
                continue;
            }
            let sent = message.at.map(|at| at.unix_secs);
            let came = now.map(|at| at.unix_secs);
            message.delivery = Some(DeliveryState::Read {
                at: kept_within(receipt.read_at, sent, came),
            });
            message.note(now, "read by the peer");
            self.changed.insert((peer, message.activity));
        }
        dropped(peer, &unknown)
    }
}

impl<B> UiModel<B> {
    /// The platform's viewport: these bubbles of `peer`'s chat are on
    /// screen.
    pub(super) fn set_viewport(&mut self, peer: [u8; 16], keys: Vec<u64>) {
        self.reading.viewport = Some((peer, keys));
    }

    /// The platform's front: a chat left behind owes its receipt now.
    pub(super) fn set_front(&mut self, front: bool) {
        self.reading.front = Some(front);
        if !front {
            for owed in self.reading.owed.values_mut() {
                owed.left = true;
            }
        }
    }

    /// A key or the mouse in the open chat.
    pub(super) fn touched(&mut self) {
        self.reading.touched = self.conversations.get(self.selected).map(|c| c.peer);
    }

    /// After every fold: what came on screen, what was seen, the receipts
    /// that ride on this fold's sends to their peer, and those due alone.
    pub(super) fn read_step(&mut self, effects: &mut Vec<UiEffect>) {
        self.watch();
        if self.reading.owed.is_empty() {
            return;
        }
        // The piggyback: the first message of the fold to a peer owed one.
        for effect in effects.iter_mut() {
            let UiEffect::Send { to, fields, .. } = effect else {
                continue;
            };
            if !self.reading.owed.contains_key(to) {
                continue;
            }
            let takes = self.takes_receipts(to);
            let Some(owed) = self.reading.owed.remove(to) else {
                continue;
            };
            if !takes {
                continue;
            }
            let mut batches = Receipt::batches(owed.read_at, &owed.ids).into_iter();
            if let Some(first) = batches.next() {
                Lev::mark_with(first).put_into(fields);
            }
            let rest: Vec<Receipt> = batches.collect();
            if !rest.is_empty() {
                let to = *to;
                self.reading.owed.insert(
                    to,
                    Owed {
                        ids: rest.into_iter().flat_map(|r| r.ids).collect(),
                        left: true,
                        ..owed
                    },
                );
            }
        }
        let now = self.elapsed_ms;
        let due: Vec<[u8; 16]> = self
            .reading
            .owed
            .iter()
            .filter(|(peer, owed)| now >= self.deadline(peer, owed))
            .map(|(peer, _)| *peer)
            .collect();
        for peer in due {
            let Some(owed) = self.reading.owed.remove(&peer) else {
                continue;
            };
            // A peer that stopped taking them since gets none.
            if !self.takes_receipts(&peer) {
                continue;
            }
            for receipt in Receipt::batches(owed.read_at, &owed.ids) {
                self.sending.push_back((RECEIPT_SEND, 0));
                effects.push(UiEffect::Send {
                    to: peer,
                    body: String::new(),
                    fields: Lev::mark_with(receipt).fields().to_vec(),
                });
            }
        }
    }

    /// When the receipt owed `peer` goes alone.
    fn deadline(&self, peer: &[u8; 16], owed: &Owed) -> u64 {
        let draft = self
            .conversations
            .iter()
            .find(|c| c.peer == *peer)
            .is_some_and(|c| !c.draft.text.trim().is_empty());
        match (draft, owed.left) {
            (true, _) => owed.last_ms.saturating_add(DRAFT_WAIT_MS),
            (false, true) => 0,
            (false, false) => owed.last_ms.saturating_add(BATCH_MS),
        }
    }

    /// Whether receipts go to `peer` at all.
    fn takes_receipts(&self, peer: &[u8; 16]) -> bool {
        self.reading.on
            && !self.home_client
            && self.names.trust(peer) != Trust::Blocked
            && self
                .conversations
                .iter()
                .find(|c| c.peer == *peer)
                .is_some_and(|c| c.flags.receipts)
    }

    /// The bubbles counting now, the clock on each, and those it made seen.
    fn watch(&mut self) {
        let now = self.elapsed_ms;
        let open = self.conversations.get(self.selected).map(|c| c.peer);
        for (peer, owed) in self.reading.owed.iter_mut() {
            if Some(*peer) != open {
                owed.left = true;
            }
        }
        let counting = self.counting();
        self.reading.since.retain(|key, _| counting.contains(key));
        for key in &counting {
            self.reading.since.entry(*key).or_insert(now);
        }
        let Some(wall) = self.now else {
            return;
        };
        let seen: Vec<u64> = self
            .reading
            .since
            .iter()
            .filter(|&(_, &since)| now.saturating_sub(since) >= SEEN_MS)
            .map(|(&key, _)| key)
            .collect();
        if seen.is_empty() {
            return;
        }
        let c = self.selected;
        let Some(conversation) = self.conversations.get_mut(c) else {
            return;
        };
        let peer = conversation.peer;
        let mut ids = Vec::new();
        let mut touched = Vec::new();
        for key in &seen {
            self.reading.since.remove(key);
            if let Some((m, message)) = conversation
                .messages
                .iter_mut()
                .enumerate()
                .find(|(_, m)| m.activity == *key)
            {
                message.note(Some(wall), SEEN);
                ids.extend(message.id);
                touched.push(m);
            }
        }
        for m in touched {
            self.touch(c, m);
        }
        let read_at = u32::try_from(wall.unix_secs.max(0)).unwrap_or(u32::MAX);
        let owed = self.reading.owed.entry(peer).or_insert(Owed {
            read_at,
            ids: Vec::new(),
            last_ms: now,
            left: false,
        });
        for id in ids {
            if !owed.ids.contains(&id) {
                owed.ids.push(id);
            }
        }
        owed.last_ms = now;
    }

    /// The keys of the open chat's bubbles that count now: on screen, in
    /// front, and a message that owes a receipt.
    fn counting(&self) -> Vec<u64> {
        let Some(conversation) = self.conversations.get(self.selected) else {
            return Vec::new();
        };
        let peer = conversation.peer;
        let front = match self.reading.front {
            Some(front) => front,
            None => self.reading.touched == Some(peer),
        };
        if !front || !self.takes_receipts(&peer) {
            return Vec::new();
        }
        let Some((shown, keys)) = &self.reading.viewport else {
            return Vec::new();
        };
        if *shown != peer {
            return Vec::new();
        }
        keys.iter()
            .copied()
            .filter(|key| {
                conversation
                    .messages
                    .iter()
                    .find(|m| m.activity == *key)
                    .is_some_and(owes_receipt)
            })
            .collect()
    }
}

/// Whether a received message is one a receipt names once seen: the
/// peer's, a bubble of its own, with an id, not seen before.
fn owes_receipt(message: &super::Message) -> bool {
    !message.outbound
        && message.id.is_some()
        && !message.is_report()
        && !message.is_reaction()
        && !message.ledger.iter().any(|entry| entry.what == SEEN)
}

/// The reader's clock, which may be wrong, kept within when the message
/// was sent and when the receipt came, where this device knows them.
fn kept_within(read_at: u32, sent: Option<i64>, came: Option<i64>) -> u32 {
    let mut at = i64::from(read_at);
    if let Some(came) = came {
        at = at.min(came);
    }
    if let Some(sent) = sent {
        at = at.max(sent);
    }
    u32::try_from(at.max(0)).unwrap_or(u32::MAX)
}

fn dropped(peer: [u8; 16], ids: &[[u8; 32]]) -> Vec<UiEffect> {
    ids.iter()
        .map(|id| {
            UiEffect::Log(format!(
                "RECEIPT dropped id={} from={}: no message of ours to that peer",
                to_hex(&id[..8]),
                to_hex(&peer[..8])
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readers_clock_is_kept_within_sent_and_came() {
        assert_eq!(kept_within(150, Some(100), Some(200)), 150);
        assert_eq!(kept_within(50, Some(100), Some(200)), 100);
        assert_eq!(kept_within(250, Some(100), Some(200)), 200);
        assert_eq!(kept_within(250, None, None), 250);
    }
}
