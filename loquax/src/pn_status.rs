//! Asking the propagation node what became of our messages (1395,
//! `docs/src/concepts/loquax-propagation-receipts.md`, section B).
//!
//! A message handed to a propagation node stays at one tick until its
//! recipient says something. A stock recipient never does, so the sender
//! asks the node it uploaded to, by the message's `transient_id`
//! ([`crate::outbox::OutboxEvent::Uploaded`]), over `/lev/status`.
//!
//! [`Asker`] is the rule of when, with no I/O:
//!
//! - only a node whose announce says it answers
//!   ([`crate::outbox::OutboxEvent::NodeAnnounced`]'s `status`); a stock
//!   node leaves the request unanswered, so it is never asked;
//! - at most once per sync round, and only while ids are open: uploaded to
//!   that node, with neither a propagation receipt, a proof nor a final
//!   answer since ([`Asker::settled`]);
//! - for at most [`MAX_AGE_SECS`] after the upload, the node's own expiry;
//! - at most [`LEV_STATUS_MAX_IDS`] ids a request, oldest first.
//!
//! The caller asks over a link of its own that it never identifies, not
//! over the sync link, which identifies before `/get`: the node must not
//! tie the asker to the ids and through them to the recipients.
//!
//! An answer closes an id at `collected`, `delivered`, `expired` and
//! `evicted`; `held` and `unknown` leave it open for the next round.
//! [`crate::ui::UiModel::pn_answered`] turns the answers into marks.
//!
//! A round that does not ask says why ([`Skip`], the log line
//! `PN_STATUS_SKIPPED reason=no_capability|no_open_ids|rate`). What is
//! open survives a restart: [`Asker::state`] is what the store keeps and
//! [`Asker::restore`] takes it back (1409), so an upload from before the
//! restart is asked about after it.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use leviculum_lxmf::lev_status::{MessageStatus, LEV_STATUS_MAX_IDS};

/// How long after its upload a message is asked about: the propagation
/// node's message expiry, 30 days (`MESSAGE_EXPIRY`,
/// `reference/LXMF/LXMF/LXMRouter.py:38`). Past it the node no longer
/// knows the id either.
pub const MAX_AGE_SECS: i64 = 30 * 24 * 3600;

/// One message the node has not settled yet: its `transient_id`, our
/// message id, the node it was uploaded to and when, unix seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub transient_id: [u8; 32],
    pub message_id: [u8; 32],
    pub node: [u8; 16],
    pub uploaded_at: i64,
}

/// What the store keeps of an [`Asker`]: the open ids and the nodes that
/// said they answer, with the version they said. The round last asked is
/// not kept: a restart may ask once more.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskerState {
    pub open: Vec<Open>,
    pub answering: Vec<([u8; 16], u64)>,
}

/// Why a round asks a node nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// Its announce never said it answers `/lev/status`: a stock node.
    NoCapability,
    /// Nothing uploaded to it is open.
    NoOpenIds,
    /// It was asked this round already.
    Rate,
}

impl Skip {
    /// The word of the log line.
    pub const fn word(self) -> &'static str {
        match self {
            Self::NoCapability => "no_capability",
            Self::NoOpenIds => "no_open_ids",
            Self::Rate => "rate",
        }
    }

    /// `PN_STATUS_SKIPPED reason=…`.
    pub fn line(self) -> String {
        format!("PN_STATUS_SKIPPED reason={}", self.word())
    }
}

/// A node's question in a round: the `transient_id`s to ask, or why not.
pub type Asked = Result<Vec<[u8; 32]>, Skip>;

/// The asking rule: what is open, which nodes answer, and the round last
/// asked.
#[derive(Clone, Debug, Default)]
pub struct Asker {
    open: BTreeMap<[u8; 32], Open>,
    answering: BTreeMap<[u8; 16], u64>,
    asked_round: BTreeMap<[u8; 16], u64>,
    /// The ids the request in flight carries, per node.
    asking: BTreeMap<[u8; 16], Vec<[u8; 32]>>,
    /// What the store keeps changed since [`Asker::take_changed`].
    changed: bool,
}

impl Asker {
    /// Uploads still asked about and their estimated heap bytes, the four
    /// maps and the ids in flight, for the mailbox's census (order 1585).
    pub fn census(&self) -> (usize, usize) {
        use leviculum_core::heap_census::btree_map_bytes;
        let asking: usize = self.asking.values().map(|ids| ids.capacity() * 32).sum();
        (
            self.open.len(),
            btree_map_bytes(&self.open)
                + btree_map_bytes(&self.answering)
                + btree_map_bytes(&self.asked_round)
                + btree_map_bytes(&self.asking)
                + asking,
        )
    }

    /// The asker a store kept.
    pub fn restore(state: AskerState) -> Self {
        Self {
            open: state
                .open
                .into_iter()
                .map(|open| (open.transient_id, open))
                .collect(),
            answering: state.answering.into_iter().collect(),
            ..Self::default()
        }
    }

    /// What the store keeps, oldest upload first.
    pub fn state(&self) -> AskerState {
        let mut open: Vec<Open> = self.open.values().cloned().collect();
        open.sort_by_key(|open| (open.uploaded_at, open.transient_id));
        AskerState {
            open,
            answering: self.answering.iter().map(|(n, v)| (*n, *v)).collect(),
        }
    }

    /// The state, once each time it changed: what to write to the store.
    pub fn take_changed(&mut self) -> Option<AskerState> {
        core::mem::take(&mut self.changed).then(|| self.state())
    }

    /// The nodes that hold an open id, each once.
    pub fn nodes(&self) -> Vec<[u8; 16]> {
        let mut nodes: Vec<[u8; 16]> = self.open.values().map(|open| open.node).collect();
        nodes.sort();
        nodes.dedup();
        nodes
    }

    /// `node` proved the upload of `message_id`, stored there as
    /// `transient_id`, at `now` (unix seconds).
    pub fn uploaded(
        &mut self,
        node: [u8; 16],
        message_id: [u8; 32],
        transient_id: [u8; 32],
        now: i64,
    ) {
        self.open.insert(
            transient_id,
            Open {
                transient_id,
                message_id,
                node,
                uploaded_at: now,
            },
        );
        self.changed = true;
    }

    /// `node` announced; `status` is the `/lev/status` version it says it
    /// answers, none for a stock node.
    pub fn announced(&mut self, node: [u8; 16], status: Option<u64>) {
        let before = match status {
            Some(version) => self.answering.insert(node, version),
            None => self.answering.remove(&node),
        };
        self.changed |= before != status;
    }

    /// Our message `message_id` needs no asking any more: a propagation
    /// receipt or a proof named it.
    pub fn settled(&mut self, message_id: &[u8; 32]) {
        let before = self.open.len();
        self.open.retain(|_, open| open.message_id != *message_id);
        self.changed |= self.open.len() != before;
    }

    /// One sync round's questions (1409): for every node that holds an open
    /// id, the ids to ask it, or why it is not asked. With nothing open at
    /// all the round says [`Skip::NoOpenIds`] once.
    pub fn round(&mut self, round: u64, now: i64) -> Vec<([u8; 16], Asked)> {
        let nodes = self.nodes();
        if nodes.is_empty() {
            return vec![([0; 16], Err(Skip::NoOpenIds))];
        }
        nodes
            .into_iter()
            .map(|node| (node, self.ask(node, round, now)))
            .collect()
    }

    /// The ids still open, for a test or a log.
    pub fn open(&self) -> usize {
        self.open.len()
    }

    /// The question for `node` in sync round `round` at `now`: the
    /// `transient_id`s to ask, or why not: the node holds nothing open, does
    /// not answer, or was asked this round. Drops the ids past
    /// [`MAX_AGE_SECS`].
    pub fn ask(&mut self, node: [u8; 16], round: u64, now: i64) -> Asked {
        let before = self.open.len();
        self.open
            .retain(|_, open| now.saturating_sub(open.uploaded_at) <= MAX_AGE_SECS);
        self.changed |= self.open.len() != before;
        let mut ids: Vec<(i64, [u8; 32])> = self
            .open
            .iter()
            .filter(|(_, open)| open.node == node)
            .map(|(id, open)| (open.uploaded_at, *id))
            .collect();
        if ids.is_empty() {
            return Err(Skip::NoOpenIds);
        }
        if !self.answering.contains_key(&node) {
            return Err(Skip::NoCapability);
        }
        if self.asked_round.get(&node) == Some(&round) {
            return Err(Skip::Rate);
        }
        ids.sort();
        let ids: Vec<[u8; 32]> = ids
            .into_iter()
            .take(LEV_STATUS_MAX_IDS)
            .map(|(_, id)| id)
            .collect();
        self.asked_round.insert(node, round);
        self.asking.insert(node, ids.clone());
        Ok(ids)
    }

    /// `node`'s answer to the question in flight: each asked id the node
    /// named, as our message id with its status, and the log line. A final
    /// word closes the id; `held` and `unknown` leave it open. `None`, a
    /// refusal or no answer at all, says nothing and leaves every id open.
    pub fn answered(
        &mut self,
        node: [u8; 16],
        answer: Option<&[([u8; 32], MessageStatus)]>,
    ) -> (Vec<([u8; 32], MessageStatus)>, String) {
        let asked = self.asking.remove(&node).unwrap_or_default();
        let mut out = Vec::new();
        for (transient_id, status) in answer.unwrap_or_default() {
            if !asked.contains(transient_id) {
                continue;
            }
            let Some(open) = self.open.get(transient_id) else {
                continue;
            };
            out.push((open.message_id, *status));
            if final_word(*status) {
                self.open.remove(transient_id);
                self.changed = true;
            }
        }
        let line = format!(
            "PN_STATUS_ASKED ids={} answered={}",
            asked.len(),
            out.iter()
                .filter(|(_, status)| *status != MessageStatus::Unknown)
                .count()
        );
        (out, line)
    }
}

/// What a `/lev/status` answer's bytes say: the statuses, or `None` for a
/// refusal or bytes that are no answer, which say nothing.
pub fn read_answer(data: &[u8]) -> Option<Vec<([u8; 32], MessageStatus)>> {
    match leviculum_lxmf::lev_status::decode_answer(data) {
        Ok(Ok(entries)) => Some(entries),
        Ok(Err(_)) | Err(_) => None,
    }
}

/// Whether a status ends the asking for its id.
pub fn final_word(status: MessageStatus) -> bool {
    matches!(
        status,
        MessageStatus::Collected
            | MessageStatus::Delivered
            | MessageStatus::Expired
            | MessageStatus::Evicted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: [u8; 16] = [0x9c; 16];
    const T0: i64 = 1_800_000_000;

    fn asker_with(n: u8) -> Asker {
        let mut asker = Asker::default();
        asker.announced(NODE, Some(1));
        for i in 0..n {
            asker.uploaded(NODE, [i; 32], [0x80 | i; 32], T0 + i64::from(i));
        }
        asker
    }

    #[test]
    fn a_stock_node_is_never_asked() {
        let mut asker = asker_with(2);
        asker.announced(NODE, None);
        assert_eq!(asker.ask(NODE, 1, T0 + 10), Err(Skip::NoCapability));
        let mut never = Asker::default();
        never.uploaded(NODE, [1; 32], [2; 32], T0);
        assert_eq!(never.ask(NODE, 1, T0 + 10), Err(Skip::NoCapability));
        assert_eq!(
            Skip::NoCapability.line(),
            "PN_STATUS_SKIPPED reason=no_capability"
        );
    }

    #[test]
    fn once_per_sync_round_and_only_the_open_ids() {
        let mut asker = asker_with(3);
        assert_eq!(
            asker.ask(NODE, 1, T0 + 10),
            Ok(vec![[0x80; 32], [0x81; 32], [0x82; 32]])
        );
        assert_eq!(
            asker.ask(NODE, 1, T0 + 20),
            Err(Skip::Rate),
            "the same round again"
        );
        // A receipt settled one, the node answered another collected.
        asker.settled(&[1; 32]);
        let (marks, line) = asker.answered(
            NODE,
            Some(&[
                ([0x80; 32], MessageStatus::Collected),
                ([0x82; 32], MessageStatus::Held),
            ]),
        );
        assert_eq!(
            marks,
            [
                ([0; 32], MessageStatus::Collected),
                ([2; 32], MessageStatus::Held)
            ]
        );
        assert_eq!(line, "PN_STATUS_ASKED ids=3 answered=2");
        assert_eq!(asker.ask(NODE, 2, T0 + 30), Ok(vec![[0x82; 32]]));
        let (_, _) = asker.answered(NODE, Some(&[([0x82; 32], MessageStatus::Expired)]));
        assert_eq!(
            asker.ask(NODE, 3, T0 + 40),
            Err(Skip::NoOpenIds),
            "nothing open"
        );
        assert_eq!(asker.open(), 0);
    }

    #[test]
    fn unknown_and_silence_leave_the_ids_open() {
        let mut asker = asker_with(1);
        assert!(asker.ask(NODE, 1, T0).is_ok());
        let (marks, line) = asker.answered(NODE, Some(&[([0x80; 32], MessageStatus::Unknown)]));
        assert_eq!(marks, [([0; 32], MessageStatus::Unknown)]);
        assert_eq!(line, "PN_STATUS_ASKED ids=1 answered=0");
        assert!(asker.ask(NODE, 2, T0).is_ok());
        let (marks, line) = asker.answered(NODE, None);
        assert!(marks.is_empty());
        assert_eq!(line, "PN_STATUS_ASKED ids=1 answered=0");
        assert_eq!(asker.open(), 1);
    }

    #[test]
    fn an_id_is_asked_for_thirty_days_and_not_a_second_longer() {
        let mut asker = asker_with(1);
        assert!(asker.ask(NODE, 1, T0 + MAX_AGE_SECS).is_ok());
        let _ = asker.answered(NODE, None);
        assert_eq!(
            asker.ask(NODE, 2, T0 + MAX_AGE_SECS + 1),
            Err(Skip::NoOpenIds)
        );
        assert_eq!(asker.open(), 0);
    }

    #[test]
    fn a_request_carries_at_most_64_ids_oldest_first() {
        let mut asker = Asker::default();
        asker.announced(NODE, Some(1));
        for i in 0..70u8 {
            asker.uploaded(NODE, [i; 32], [i; 32], T0 + 100 - i64::from(i));
        }
        let ids = asker.ask(NODE, 1, T0 + 200).unwrap_or_default();
        assert_eq!(ids.len(), LEV_STATUS_MAX_IDS);
        assert_eq!(ids[0], [69; 32], "the oldest upload first");
    }

    #[test]
    fn an_answer_for_an_id_not_asked_is_passed_over() {
        let mut asker = asker_with(1);
        let other = [0x55; 16];
        asker.announced(other, Some(1));
        asker.uploaded(other, [7; 32], [0x77; 32], T0);
        assert!(asker.ask(NODE, 1, T0).is_ok());
        let (marks, _) = asker.answered(NODE, Some(&[([0x77; 32], MessageStatus::Collected)]));
        assert!(marks.is_empty());
        assert_eq!(asker.open(), 2);
    }

    #[test]
    fn what_is_open_survives_a_restart_and_is_asked_after_it() {
        let mut asker = asker_with(2);
        let kept = asker.take_changed().expect("uploads change what is kept");
        assert_eq!(asker.take_changed(), None, "said once");
        assert_eq!(kept.open.len(), 2);
        assert_eq!(kept.answering, [(NODE, 1)]);
        let mut again = Asker::restore(kept.clone());
        assert_eq!(again.state(), kept);
        assert_eq!(again.take_changed(), None, "a restore changes nothing");
        assert_eq!(again.nodes(), [NODE]);
        assert_eq!(again.ask(NODE, 1, T0 + 5), Ok(vec![[0x80; 32], [0x81; 32]]));
        let _ = again.answered(NODE, Some(&[([0x80; 32], MessageStatus::Collected)]));
        let after = again.take_changed().expect("a final word changes it");
        assert_eq!(after.open.len(), 1);
        assert_eq!(after.open[0].message_id, [1; 32]);
    }

    #[test]
    fn a_held_answer_and_a_repeated_announce_change_nothing_kept() {
        let mut asker = asker_with(1);
        let _ = asker.take_changed();
        asker.announced(NODE, Some(1));
        assert!(asker.ask(NODE, 1, T0).is_ok());
        let _ = asker.answered(NODE, Some(&[([0x80; 32], MessageStatus::Held)]));
        assert_eq!(asker.take_changed(), None);
        asker.announced(NODE, None);
        assert!(asker.take_changed().is_some(), "the node stopped answering");
    }

    #[test]
    fn an_answer_reads_from_its_bytes_and_a_refusal_says_nothing() {
        let answer =
            leviculum_lxmf::lev_status::encode_answer(&[([0x80; 32], MessageStatus::Collected)]);
        assert_eq!(
            read_answer(&answer),
            Some(vec![([0x80; 32], MessageStatus::Collected)])
        );
        let refusal = leviculum_lxmf::lev_status::encode_refusal(
            leviculum_lxmf::propagation::PeerError::Throttled,
        );
        assert_eq!(read_answer(&refusal), None);
        assert_eq!(read_answer(&[0xc1]), None);
    }

    /// The question on the wire for one upload: an array of one 32-byte
    /// bin, 35 bytes (`docs/src/concepts/loquax-propagation-receipts.md`,
    /// section B).
    #[test]
    fn the_request_for_one_upload_is_35_bytes() {
        let mut asker = asker_with(1);
        let ids = asker.ask(NODE, 1, T0).unwrap_or_default();
        let request = leviculum_lxmf::lev_status::encode_request(&ids);
        let mut want = vec![0x91, 0xc4, 0x20];
        want.extend([0x80; 32]);
        assert_eq!(request, want);
    }
}
