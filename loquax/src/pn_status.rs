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

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::lev_status::{MessageStatus, LEV_STATUS_MAX_IDS};

/// How long after its upload a message is asked about: the propagation
/// node's message expiry, 30 days (`MESSAGE_EXPIRY`,
/// `reference/LXMF/LXMF/LXMRouter.py:38`). Past it the node no longer
/// knows the id either.
pub const MAX_AGE_SECS: i64 = 30 * 24 * 3600;

/// One message the node has not settled yet.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Open {
    message_id: [u8; 32],
    node: [u8; 16],
    uploaded_at: i64,
}

/// The asking rule: what is open, which nodes answer, and the round last
/// asked.
#[derive(Clone, Debug, Default)]
pub struct Asker {
    open: BTreeMap<[u8; 32], Open>,
    answering: BTreeMap<[u8; 16], u64>,
    asked_round: BTreeMap<[u8; 16], u64>,
    /// The ids the request in flight carries, per node.
    asking: BTreeMap<[u8; 16], Vec<[u8; 32]>>,
}

impl Asker {
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
                message_id,
                node,
                uploaded_at: now,
            },
        );
    }

    /// `node` announced; `status` is the `/lev/status` version it says it
    /// answers, none for a stock node.
    pub fn announced(&mut self, node: [u8; 16], status: Option<u64>) {
        match status {
            Some(version) => {
                self.answering.insert(node, version);
            }
            None => {
                self.answering.remove(&node);
            }
        }
    }

    /// Our message `message_id` needs no asking any more: a propagation
    /// receipt or a proof named it.
    pub fn settled(&mut self, message_id: &[u8; 32]) {
        self.open.retain(|_, open| open.message_id != *message_id);
    }

    /// The ids still open, for a test or a log.
    pub fn open(&self) -> usize {
        self.open.len()
    }

    /// The question for `node` in sync round `round` at `now`: the
    /// `transient_id`s to ask, or none when the node does not answer, was
    /// asked this round, or holds nothing open. Drops the ids past
    /// [`MAX_AGE_SECS`].
    pub fn ask(&mut self, node: [u8; 16], round: u64, now: i64) -> Option<Vec<[u8; 32]>> {
        self.open
            .retain(|_, open| now.saturating_sub(open.uploaded_at) <= MAX_AGE_SECS);
        if !self.answering.contains_key(&node) || self.asked_round.get(&node) == Some(&round) {
            return None;
        }
        let mut ids: Vec<(i64, [u8; 32])> = self
            .open
            .iter()
            .filter(|(_, open)| open.node == node)
            .map(|(id, open)| (open.uploaded_at, *id))
            .collect();
        if ids.is_empty() {
            return None;
        }
        ids.sort();
        let ids: Vec<[u8; 32]> = ids
            .into_iter()
            .take(LEV_STATUS_MAX_IDS)
            .map(|(_, id)| id)
            .collect();
        self.asked_round.insert(node, round);
        self.asking.insert(node, ids.clone());
        Some(ids)
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

/// Whether a status ends the asking for its id.
fn final_word(status: MessageStatus) -> bool {
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
        assert_eq!(asker.ask(NODE, 1, T0 + 10), None);
        let mut never = Asker::default();
        never.uploaded(NODE, [1; 32], [2; 32], T0);
        assert_eq!(never.ask(NODE, 1, T0 + 10), None);
    }

    #[test]
    fn once_per_sync_round_and_only_the_open_ids() {
        let mut asker = asker_with(3);
        assert_eq!(
            asker.ask(NODE, 1, T0 + 10),
            Some(vec![[0x80; 32], [0x81; 32], [0x82; 32]])
        );
        assert_eq!(asker.ask(NODE, 1, T0 + 20), None, "the same round again");
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
        assert_eq!(asker.ask(NODE, 2, T0 + 30), Some(vec![[0x82; 32]]));
        let (_, _) = asker.answered(NODE, Some(&[([0x82; 32], MessageStatus::Expired)]));
        assert_eq!(asker.ask(NODE, 3, T0 + 40), None, "nothing open");
        assert_eq!(asker.open(), 0);
    }

    #[test]
    fn unknown_and_silence_leave_the_ids_open() {
        let mut asker = asker_with(1);
        assert!(asker.ask(NODE, 1, T0).is_some());
        let (marks, line) = asker.answered(NODE, Some(&[([0x80; 32], MessageStatus::Unknown)]));
        assert_eq!(marks, [([0; 32], MessageStatus::Unknown)]);
        assert_eq!(line, "PN_STATUS_ASKED ids=1 answered=0");
        assert!(asker.ask(NODE, 2, T0).is_some());
        let (marks, line) = asker.answered(NODE, None);
        assert!(marks.is_empty());
        assert_eq!(line, "PN_STATUS_ASKED ids=1 answered=0");
        assert_eq!(asker.open(), 1);
    }

    #[test]
    fn an_id_is_asked_for_thirty_days_and_not_a_second_longer() {
        let mut asker = asker_with(1);
        assert!(asker.ask(NODE, 1, T0 + MAX_AGE_SECS).is_some());
        let _ = asker.answered(NODE, None);
        assert_eq!(asker.ask(NODE, 2, T0 + MAX_AGE_SECS + 1), None);
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
        assert!(asker.ask(NODE, 1, T0).is_some());
        let (marks, _) = asker.answered(NODE, Some(&[([0x77; 32], MessageStatus::Collected)]));
        assert!(marks.is_empty());
        assert_eq!(asker.open(), 2);
    }
}
