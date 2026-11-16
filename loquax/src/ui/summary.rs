//! What the chats list asks of a conversation, kept beside it (1463): when
//! its newest message was written, how many unread rows it has, the
//! avatar's appearance, where its newest position reports are, and which
//! message carries an id (the dedupe of every arrival and every pulled
//! sent entry, which walked them all once per entry). Each of them is a walk over every message when
//! asked cold, and the list asks them for every row on every draw; on the
//! M9 (PSRAM, 4067 entries) that walk was three seconds a key.
//!
//! [`super::update`] brings every conversation's summary up to its
//! messages after each event: messages only ever come at the end (an
//! arrival, a send) or the conversation changes as a whole (a history
//! page, a removal), so the summary reads the new tail and nothing else,
//! or starts over when the conversation's ends are not the ones it saw.
//! A reader that finds the summary behind its messages (a model changed
//! outside `update`, as tests and the history loader do) walks them as
//! before: the summary is a cache, never the truth.
//!
//! A message's id is the one field that changes in place: a send is named
//! when the router or the home takes it. The two places that name one
//! (`on_engine`'s `Queued`, `home_sent`'s running send) call
//! [`Summary::forget`], and the next refresh reads the conversation anew.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::Message;

/// What identifies a message for the summary: its activity and its time,
/// neither of which changes once the message is in a conversation (its id
/// does: a send is named when the home takes it).
type Stamp = (u64, Option<i64>);

fn stamp(message: &Message) -> Stamp {
    (message.activity, message.at.map(|at| at.unix_secs))
}

/// The messages a summary was made over: how many, the first and the last.
#[derive(Clone, Debug)]
struct Seen {
    len: usize,
    first: Option<Stamp>,
    last: Option<Stamp>,
}

impl Seen {
    fn of(messages: &[Message]) -> Self {
        Self {
            len: messages.len(),
            first: messages.first().map(stamp),
            last: messages.last().map(stamp),
        }
    }

    /// Whether `messages` are the ones seen, or those and more after them.
    fn is_prefix_of(&self, messages: &[Message]) -> bool {
        match self.len {
            0 => true,
            n => {
                n <= messages.len()
                    && messages.first().map(stamp) == self.first
                    && Some(stamp(&messages[n - 1])) == self.last
            }
        }
    }
}

/// A conversation's list summary. Equal to every other: it is a cache, and
/// two conversations with the same messages are the same conversation
/// whatever their caches hold.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    seen: Option<Seen>,
    /// The newest time of a message that moves the list (no report, no
    /// group state), and of any message.
    moves: Option<i64>,
    any: Option<i64>,
    /// One per received message that has a row of its own (no reaction,
    /// no group state), oldest arrival first (1505: the body runs in its
    /// writers' order, the unread are the newest by arrival): its key and
    /// how many of them up to this one are no position report. The unread
    /// rows of an unread count `n` are the last entry less the one `n`
    /// before it.
    received: Vec<(u64, u32)>,
    /// The newest received icon appearance, foreground and background.
    appearance: Option<(String, String)>,
    /// The indices of the newest position reports with a fix, oldest
    /// first, [`super::liveness::CADENCE_GAPS`] and one of them: what the
    /// peer's liveness reads.
    reports: Vec<usize>,
    /// Each id a message carries, and which messages carry it.
    ids: BTreeMap<[u8; 32], Vec<usize>>,
    /// A [`Summary::catch_up`] read messages the next
    /// [`Summary::refresh`] has not reported yet.
    unnoted: bool,
}

impl PartialEq for Summary {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Summary {}

impl Summary {
    /// Bring the summary up to `messages`: their new tail, or all of them
    /// when they are not the ones seen with more after. Whether anything
    /// was read: the conversation's messages changed since the last
    /// refresh (1490, the revision counter's detector).
    pub fn refresh(&mut self, peer: [u8; 16], messages: &[Message]) -> bool {
        let read = self.read(peer, messages);
        read | core::mem::take(&mut self.unnoted)
    }

    /// Bring the summary up to `messages` inside a fold (1521): a
    /// batch's folds look ids up before its one note, and a summary
    /// behind them walked the whole conversation per entry. What it read
    /// is still the next [`Summary::refresh`]'s to report, so the note
    /// sees the change as before.
    pub fn catch_up(&mut self, peer: [u8; 16], messages: &[Message]) {
        if self.read(peer, messages) {
            self.unnoted = true;
        }
    }

    fn read(&mut self, peer: [u8; 16], messages: &[Message]) -> bool {
        let from = match &self.seen {
            Some(seen) if seen.is_prefix_of(messages) => seen.len,
            _ => {
                let unnoted = self.unnoted;
                *self = Self::default();
                self.unnoted = unnoted;
                0
            }
        };
        if from == messages.len() && self.seen.is_some() {
            return false;
        }
        let mut disorder = false;
        for (index, message) in messages.iter().enumerate().skip(from) {
            if message.is_report() && super::peers_own_fix(peer, message).is_some() {
                if self.reports.len() > super::liveness::CADENCE_GAPS {
                    self.reports.remove(0);
                }
                self.reports.push(index);
            }
            let at = message.at.map(|at| at.unix_secs);
            self.any = self.any.max(at);
            if !(message.is_report() || message.is_group_state()) {
                self.moves = self.moves.max(at);
            }
            if !message.outbound && !message.is_aside() {
                match self.received.last() {
                    Some(&(key, _)) if key > message.activity => disorder = true,
                    last => {
                        let before = last.map_or(0, |&(_, n)| n);
                        self.received
                            .push((message.activity, before + u32::from(!message.is_report())));
                    }
                }
            }
            if let Some(id) = message.id {
                self.ids.entry(id).or_default().push(index);
            }
            if !message.outbound {
                if let Some(found) = super::chat::appearance(&message.fields) {
                    self.appearance = Some(found);
                }
            }
        }
        if disorder {
            self.received = by_arrival(messages);
        }
        self.seen = Some(Seen::of(messages));
        true
    }

    /// Start over at the next refresh: a message's id changed in place.
    pub fn forget(&mut self) {
        self.seen = None;
    }

    /// The messages that carry `id`, oldest first: `None` when the summary
    /// is behind `messages`.
    pub fn with_id(&self, messages: &[Message], id: &[u8; 32]) -> Option<&[usize]> {
        self.fresh(messages)
            .then(|| self.ids.get(id).map_or(&[][..], Vec::as_slice))
    }

    /// Whether the summary was made over exactly `messages`.
    fn fresh(&self, messages: &[Message]) -> bool {
        self.seen.as_ref().is_some_and(|seen| {
            seen.len == messages.len()
                && seen.first == messages.first().map(stamp)
                && seen.last == messages.last().map(stamp)
        })
    }

    /// The newest time of a message that moves the list, else of any:
    /// `None` when the summary is behind `messages`.
    pub fn newest(&self, messages: &[Message]) -> Option<Option<i64>> {
        self.fresh(messages).then(|| self.moves.or(self.any))
    }

    /// The unread rows of `unread` received messages: `None` when the
    /// summary is behind `messages`.
    pub fn unread_rows(&self, messages: &[Message], unread: usize) -> Option<usize> {
        if !self.fresh(messages) {
            return None;
        }
        let total = self.received.last().map_or(0, |&(_, n)| n);
        let before = match self.received.len().checked_sub(unread + 1) {
            Some(i) => self.received[i].1,
            None => 0,
        };
        Some((total - before) as usize)
    }

    /// The indices of the newest position reports with a fix, newest
    /// first: `None` when the summary is behind `messages`.
    pub fn reports(&self, messages: &[Message]) -> Option<impl Iterator<Item = usize> + '_> {
        self.fresh(messages)
            .then(|| self.reports.iter().rev().copied())
    }

    /// The received messages it summed, each that has a row of its own
    /// or not: what [`Summary::unread_rows`] counts `unread` back over.
    pub fn received_count(&self) -> usize {
        self.received.len()
    }

    /// The newest received icon appearance: `None` when the summary is
    /// behind `messages`.
    pub fn appearance(&self, messages: &[Message]) -> Option<Option<(String, String)>> {
        self.fresh(messages).then(|| self.appearance.clone())
    }
}

/// The received messages with a row of their own, oldest arrival first,
/// each with how many up to it are no position report: a body whose
/// writers' order is not its arrival order (1505), counted anew.
fn by_arrival(messages: &[Message]) -> Vec<(u64, u32)> {
    let mut received: Vec<(u64, bool)> = messages
        .iter()
        .filter(|m| !m.outbound && !m.is_aside())
        .map(|m| (m.activity, !m.is_report()))
        .collect();
    received.sort_by_key(|&(key, _)| key);
    let mut count = 0;
    received
        .into_iter()
        .map(|(key, text)| {
            count += u32::from(text);
            (key, count)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::Summary;
    use crate::outbox::OutboxEvent;
    use crate::ui::tests::Field;
    use crate::ui::{update, Conversation, UiEvent, UiModel};
    use crate::when::WallTime;

    const PEERS: [[u8; 16]; 3] = [[1; 16], [2; 16], [3; 16]];

    fn received(model: &mut UiModel<Field>, n: u32) {
        let peer = PEERS[n as usize % PEERS.len()];
        // Every seventh a position report without text (1209).
        let (body, fields) = match n % 7 {
            3 => (
                Vec::new(),
                crate::ui::position_fields(leviculum_lxmf::telemetry::Location {
                    latitude_e6: 43_770_000 + n as i32,
                    longitude_e6: 11_250_000,
                    altitude_e2: 0,
                    speed_e2: 0,
                    bearing_e2: 0,
                    accuracy_e2: 500,
                    last_update: 1_762_000_000 + i64::from(n),
                }),
            ),
            _ => (alloc::format!("message {n}").into_bytes(), Vec::new()),
        };
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(&n.to_le_bytes());
        update(
            model,
            UiEvent::Clock(WallTime {
                unix_secs: 1_762_000_000 + i64::from(n),
                offset_secs: 0,
            }),
        );
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 1_762_000_000 + i64::from(n),
                message_id: id,
                source: peer,
                title: Vec::new(),
                body,
                fields,
                verified: true,
            }),
        );
    }

    /// The conversation as a reader that walks every message sees it.
    fn cold(conversation: &Conversation) -> Conversation {
        let mut cold = conversation.clone();
        cold.summary = Summary::default();
        cold
    }

    fn agrees(conversation: &Conversation) {
        let cold = cold(conversation);
        assert!(
            conversation
                .summary
                .newest(&conversation.messages)
                .is_some(),
            "the summary is up to its messages"
        );
        assert_eq!(conversation.last_written(), cold.last_written());
        assert_eq!(conversation.appearance(), cold.appearance());
        for now in [None, Some(1_762_000_030), Some(1_762_100_000)] {
            assert_eq!(conversation.liveness(now), cold.liveness(now));
        }
        let mut both = (conversation.clone(), cold);
        for unread in [0, 1, 2, 5, conversation.messages.len(), 10_000] {
            both.0.unread = unread;
            both.1.unread = unread;
            assert_eq!(
                both.0.unread_rows(),
                both.1.unread_rows(),
                "unread {unread}"
            );
        }
    }

    /// 1463: the summary `update` keeps reads what a walk over every
    /// message reads, as the messages arrive one by one, and after the
    /// conversation is cut from the front (a removal the summary must not
    /// extend over).
    #[test]
    fn the_summary_agrees_with_a_walk_over_every_message() {
        let mut model = UiModel::<Field>::new(&[]);
        for n in 0..150 {
            received(&mut model, n);
            for conversation in &model.conversations {
                agrees(conversation);
            }
        }
        assert_eq!(model.conversations.len(), PEERS.len());
        // Cut from the front: the ends the summary saw are gone, so it is
        // behind until the next update rebuilds it.
        model.conversations[0].messages.remove(0);
        let first = &model.conversations[0];
        assert!(first.summary.newest(&first.messages).is_none());
        assert_eq!(first.last_written(), cold(first).last_written());
        received(&mut model, 150);
        for conversation in &model.conversations {
            agrees(conversation);
        }
    }

    /// 1490: an arrival bumps the revision of the conversation it fell
    /// into and of no other; an event that changes nothing visible bumps
    /// none; the minute moves the model's look, not the conversations.
    #[test]
    fn a_change_bumps_its_conversations_revision_and_only_that() {
        let mut model = UiModel::<Field>::new(&[]);
        for n in 0..6 {
            received(&mut model, n);
        }
        let before: Vec<u64> = model.conversations.iter().map(|c| c.revision()).collect();
        let look = model.look_rev();
        // The next arrival goes to PEERS[0] (n % 3).
        received(&mut model, 6);
        assert_eq!(model.conversations[0].revision(), before[0] + 1);
        assert_eq!(model.conversations[1].revision(), before[1]);
        assert_eq!(model.conversations[2].revision(), before[2]);
        // A tick changes nothing visible of any conversation.
        let before: Vec<u64> = model.conversations.iter().map(|c| c.revision()).collect();
        update(&mut model, UiEvent::Tick);
        let after: Vec<u64> = model.conversations.iter().map(|c| c.revision()).collect();
        assert_eq!(before, after);
        // The clock a minute on moves the look and no conversation.
        update(
            &mut model,
            UiEvent::Clock(WallTime {
                unix_secs: 1_762_000_000 + 3_600,
                offset_secs: 0,
            }),
        );
        assert_eq!(
            model
                .conversations
                .iter()
                .map(|c| c.revision())
                .collect::<Vec<u64>>(),
            after
        );
        assert!(model.look_rev() > look, "the minute moved the look");
    }

    /// 1490: the fold-maintained order is the full sort's, after every
    /// arrival, after a pin, an archive and a block, and even when the
    /// model was changed behind `update`'s back (the cache checks its
    /// keys before it answers; a stale cache sorts afresh).
    #[test]
    fn the_cached_order_is_the_full_sorts() {
        let mut model = UiModel::<Field>::new(&[]);
        for n in 0..60 {
            received(&mut model, n);
            assert_eq!(model.order(), model.order_cold(), "after arrival {n}");
        }
        model.conversations[1].pinned = true;
        update(&mut model, UiEvent::Tick);
        assert_eq!(model.order(), model.order_cold(), "after the pin");
        assert_eq!(model.order().first(), Some(&1), "the pinned leads");
        model.conversations[2].flags.archived = true;
        update(&mut model, UiEvent::Tick);
        assert_eq!(model.order(), model.order_cold(), "after the archive");
        assert_eq!(model.order().len(), 2, "the archived left the list");
        let blocked = model.conversations[0].peer;
        model.names.contacts.blocked.insert(blocked);
        update(&mut model, UiEvent::Tick);
        assert_eq!(model.order(), model.order_cold(), "after the block");
        assert_eq!(model.order(), [1], "only the pinned remains");
        // The positive control: a change update() never saw. The read
        // checks the keys and sorts afresh instead of answering stale.
        model.conversations[1].pinned = false;
        model.names.contacts.blocked.remove(&blocked);
        assert_eq!(model.order(), model.order_cold(), "behind update's back");
        // Peer 1 heard arrival 58, peer 0 arrival 57: newest first.
        assert_eq!(model.order(), [1, 0]);
    }

    /// The positive control: a summary left behind its messages (an
    /// arrival `update` did not see) is not trusted, and the walk answers.
    #[test]
    fn a_summary_behind_its_messages_is_not_trusted() {
        let mut model = UiModel::<Field>::new(&[]);
        for n in 0..6 {
            received(&mut model, n);
        }
        let mut stale = model.conversations[0].clone();
        let mut newer = stale.messages[0].clone();
        newer.activity = 999;
        newer.at = Some(WallTime {
            unix_secs: 1_800_000_000,
            offset_secs: 0,
        });
        stale.messages.push(newer);
        assert!(stale.summary.newest(&stale.messages).is_none());
        assert_eq!(stale.last_written(), Some(1_800_000_000));
    }
}
