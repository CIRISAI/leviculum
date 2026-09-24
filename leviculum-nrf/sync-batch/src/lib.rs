#![no_std]
//! The inbound peer-sync batch on the board: what one drain step counts, and
//! what a `LinkClosed` under a half-drained batch means.
//!
//! # The defect this crate pins (`lora_pn_board_offer_past_the_link`, 2026-09-24)
//!
//! A peer-sync round on this stack has two halves that finish at very
//! different times. The SENDER is done when the resource is proven: the
//! reference's `resource_concluded` moves every transferred id from unhandled
//! to handled and tears the link down in the same block
//! (`reference/LXMF/LXMF/LXMPeer.py:499-503`), and
//! `leviculum_nrf::pn::Engine::conclude_round` does the same. The RECEIVER is
//! not: at the announced stamp cost the whole batch is resident in RAM while
//! each message walks its 1000-round workblock, about 3.7 s per message on an
//! nRF52840 (`leviculum-settle-budget`).
//!
//! So the sender's LINKCLOSE lands in the middle of the receiver's grind. The
//! board treated that close as "the sender is gone" and concluded the batch —
//! dropping every message it had not yet judged. The rig window measured it
//! three times in one run: offered 7, kept 1; offered 10, kept 1; offered 7,
//! kept 7, the last only because its LINKCLOSE was cut mid-preamble and never
//! arrived. The grep-able signature is the two `PN_SYNC ... result=ok` lines
//! of one round disagreeing on `transferred=` (7 on the sender, 1 here).
//!
//! The docker twin cannot see it: on a host the validation finishes before the
//! close arrives, so the tail is judged either way.
//!
//! # The rule
//!
//! A close decides nothing about messages that are already in our hands.
//! [`SyncBatch::after_close`] therefore answers [`AfterClose::KeepDraining`]
//! for a batch that still holds messages: they are resident, the stamps need
//! only CPU, and the link is needed for nothing further. The link's own
//! bookkeeping (pending proofs, validated links, inbound transfers) is cleared
//! by the caller either way — that state IS about the link.
//!
//! [`AfterClose::Conclude`] is left for the case the abandon path was written
//! for: a batch with nothing left to judge. A link that dies while the
//! resource is still in flight never reaches this type at all — the batch is
//! built from the decoded envelope after the transfer concluded, so there is
//! no batch to abandon.
//!
//! # What is modelled here, and what is not
//!
//! This crate is the batch's bookkeeping and that one decision. It does not
//! hash anything and does not know what a message is: the firmware crate
//! cross-compiles to `thumbv7em-none-eabihf` and has no test target, which is
//! why every decision in it that can be made a pure function lives in a
//! sibling crate like this one. The engine's drain step
//! (`leviculum_nrf::pn::Engine::perform_sync_step`) is the sequence of calls
//! this crate's tests make, in the same order.

extern crate alloc;

use alloc::collections::VecDeque;

/// What a `LinkClosed` for the batch's own link means for the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterClose {
    /// Messages are still resident and unjudged: keep grinding them. The
    /// sender is finished, not gone.
    KeepDraining,
    /// Nothing left to judge: log the round and drop the batch.
    Conclude,
}

/// One inbound sync batch, draining one message per settle pass.
///
/// Generic over the message body so the tests can drain byte strings while the
/// firmware drains `Vec<u8>` envelopes — the queue lives HERE rather than
/// beside a count in the engine, so there is no second copy of "how many are
/// left" to drift away from the messages themselves.
pub struct SyncBatch<M> {
    messages: VecDeque<M>,
    accepted: usize,
    bytes: u64,
    invalid: usize,
}

impl<M> SyncBatch<M> {
    /// Take the batch of an `/offer` whose envelope decoded.
    pub fn new(messages: VecDeque<M>) -> Self {
        Self {
            messages,
            accepted: 0,
            bytes: 0,
            invalid: 0,
        }
    }

    /// The next message to judge, removed from the queue.
    pub fn take_next(&mut self) -> Option<M> {
        self.messages.pop_front()
    }

    /// Put back a message whose stamp workblock is only part-expanded.
    ///
    /// The FRONT, so the next pass resumes the same workblock: a queue that
    /// reordered under a parked grind would judge one message's stamp by
    /// another message's workblock.
    pub fn park(&mut self, message: M) {
        self.messages.push_front(message);
    }

    /// Count a message that was stored durably and was not already held.
    pub fn accept(&mut self, bytes: u64) {
        self.accepted += 1;
        self.bytes += bytes;
    }

    /// Count a message whose stamp did not hold up, or that would not decode.
    pub fn reject(&mut self) {
        self.invalid += 1;
    }

    /// No message left to judge.
    pub fn drained(&self) -> bool {
        self.messages.is_empty()
    }

    /// The `transferred=` of the `PN_SYNC dir=in` line: messages newly stored,
    /// which is neither the number offered nor the number judged.
    pub fn transferred(&self) -> usize {
        self.accepted
    }

    /// The `bytes=` of the `PN_SYNC dir=in` line.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// How many of the judged messages were refused; non-zero throttles the
    /// sender.
    pub fn invalid(&self) -> usize {
        self.invalid
    }

    /// The `result=` of the `PN_SYNC dir=in` line.
    pub fn result(&self) -> &'static str {
        if self.invalid == 0 {
            "ok"
        } else {
            "invalid_stamps"
        }
    }

    /// The messages still held, for the heap census.
    pub fn messages(&self) -> &VecDeque<M> {
        &self.messages
    }

    /// The rule this crate exists for — see the module docs.
    ///
    /// Until eb9b8b7a this was unconditionally [`AfterClose::Conclude`], which
    /// is what cut six of seven messages off a round that had crossed the air
    /// whole.
    pub fn after_close(&self) -> AfterClose {
        if self.drained() {
            AfterClose::Conclude
        } else {
            AfterClose::KeepDraining
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Drain what is left of a batch the way `perform_sync_step` does, one
    /// message per pass, every stamp holding up.
    fn settle_until_drained(batch: &mut SyncBatch<&'static [u8]>) {
        while let Some(message) = batch.take_next() {
            batch.accept(message.len() as u64);
        }
    }

    /// The minimal reproduction of the 2026-09-24 rig defect: three messages
    /// resident, one judged, and the sender's LINKCLOSE arriving in the middle
    /// of the second one's workblock. All three are ours to judge, and the
    /// round transferred three.
    #[test]
    fn a_close_under_a_resident_batch_does_not_cut_its_tail() {
        let mut batch = SyncBatch::new(VecDeque::from(vec![
            &b"first"[..],
            &b"second"[..],
            &b"third"[..],
        ]));

        let first = batch.take_next().expect("a message to judge");
        batch.accept(first.len() as u64);

        assert_eq!(batch.after_close(), AfterClose::KeepDraining);

        settle_until_drained(&mut batch);
        assert!(batch.drained());
        assert_eq!(batch.transferred(), 3);
        assert_eq!(batch.bytes(), 16);
        assert_eq!(batch.result(), "ok");
    }

    /// The parked grind is the same case: the message is back at the front and
    /// still unjudged, so the close does not end it.
    #[test]
    fn a_close_over_a_parked_workblock_keeps_the_message() {
        let mut batch = SyncBatch::new(VecDeque::from(vec![&b"first"[..], &b"second"[..]]));

        let first = batch.take_next().expect("a message to judge");
        batch.park(first);

        assert_eq!(batch.after_close(), AfterClose::KeepDraining);
        settle_until_drained(&mut batch);
        assert_eq!(batch.transferred(), 2);
    }

    /// The case the abandon path was written for: nothing left to judge, so
    /// the close concludes the round rather than leaving a spent batch
    /// standing in the way of the next `/offer`.
    #[test]
    fn a_close_over_a_spent_batch_concludes_it() {
        let mut batch = SyncBatch::new(VecDeque::from(vec![&b"only"[..]]));
        settle_until_drained(&mut batch);

        assert_eq!(batch.after_close(), AfterClose::Conclude);
        assert_eq!(batch.transferred(), 1);
    }

    /// `transferred=` counts what was stored, not what was judged: a refused
    /// stamp moves `result=` and nothing else.
    #[test]
    fn a_refused_stamp_shows_in_the_result_not_in_the_transfer() {
        let mut batch = SyncBatch::new(VecDeque::from(vec![&b"good"[..], &b"bad"[..]]));

        let good = batch.take_next().expect("a message to judge");
        batch.accept(good.len() as u64);
        let _bad = batch.take_next().expect("a message to judge");
        batch.reject();

        assert!(batch.drained());
        assert_eq!(batch.transferred(), 1);
        assert_eq!(batch.bytes(), 4);
        assert_eq!(batch.invalid(), 1);
        assert_eq!(batch.result(), "invalid_stamps");
    }
}
