//! Active delivery from a propagation node: which stored message the node
//! pushes when a destination becomes reachable, and what a failed push
//! leaves behind.
//!
//! The board's engine (`leviculum-nrf/src/pn.rs`, `trigger_active` and the
//! `DeliveryFailed` arm of `on_event`) sends a stored message to its
//! recipient as one opportunistic packet the moment it hears that recipient
//! announce or finds a path to it, without waiting for a `/get`. Both
//! decisions here were inline there until leviculum#413, reachable only by
//! `lora_pn_board_sync` (an 1800 s cell on four radios) for the pick and by
//! no cell at all for the give-up, since none makes a push fail.
//!
//! One message is in flight at a time, so the pick names exactly one.
//!
//! The third decision, what a push the core refuses outright leaves
//! ([`after_refused_push`], [`remember_too_large`]), followed in the third
//! wave of leviculum#413: no cell makes the core refuse a push.

use alloc::vec::Vec;

use leviculum_core::SendError;

use crate::propagation::TransientId;
use crate::propagation_store::StoredMessage;

/// The stored message to push to one destination: the OLDEST one addressed
/// to it (lowest store sequence), skipping ids already known not to fit a
/// single packet.
///
/// A fold, fed one directory entry at a time from the store's own scan
/// (`PropagationStore::for_each`), so the board builds no list.
#[derive(Debug, Clone)]
pub struct ActiveCandidate<'a> {
    destination: [u8; 16],
    excluded: &'a [TransientId],
    best: Option<StoredMessage>,
}

impl<'a> ActiveCandidate<'a> {
    /// A pick for `destination` that never chooses an id in `excluded` (the
    /// node's too-large list: messages left for `/get`, never pushed).
    pub fn new(destination: [u8; 16], excluded: &'a [TransientId]) -> Self {
        Self {
            destination,
            excluded,
            best: None,
        }
    }

    /// Offer one directory entry.
    pub fn consider(&mut self, meta: &StoredMessage) {
        if meta.destination_hash == self.destination
            && !self.excluded.contains(&meta.transient_id)
            && self.best.is_none_or(|held| meta.sequence < held.sequence)
        {
            self.best = Some(*meta);
        }
    }

    /// The message to push, or `None` when nothing pushable is stored for
    /// the destination.
    pub fn best(self) -> Option<StoredMessage> {
        self.best
    }
}

/// What a push the core reported as failed (`NodeEvent::DeliveryFailed`)
/// leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailedPush {
    /// The message's attempts are spent: stop pushing it. It stays in the
    /// store for the recipient's `/get`, and the next trigger for the same
    /// destination starts a fresh budget.
    GiveUp,
    /// Push the same message again with this many attempts left.
    Resend {
        /// Attempts left after the one that failed.
        attempts_left: u8,
    },
}

/// Count the failed attempt against `attempts_left`, the budget the push
/// went out with: the last one gives up, any earlier one resends.
pub const fn after_failed_push(attempts_left: u8) -> AfterFailedPush {
    match attempts_left.saturating_sub(1) {
        0 => AfterFailedPush::GiveUp,
        left => AfterFailedPush::Resend {
            attempts_left: left,
        },
    }
}

/// How many ids the too-large list holds before the oldest drops out.
pub const TOO_LARGE_CAP: usize = 64;

/// What a push the core refused to send at all leaves (the `Err` arm of
/// the board's `send_raw_single_packet`, `resend_active` in
/// `leviculum-nrf/src/pn.rs`). A push the core accepted is the one in
/// flight; nothing here decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterRefusedPush {
    /// The message does not fit one packet: remember it
    /// ([`remember_too_large`]) so the pick never names it again. It stays
    /// in the store for the recipient's `/get`, and no gap is set, so the
    /// next-oldest can go at once.
    LeaveForGet,
    /// No path after all, or pacing: wait the engine's gap, then the next
    /// trigger retries the same pick.
    WaitGap,
}

/// Map the core's refusal of a push to what the engine does next.
pub const fn after_refused_push(error: &SendError) -> AfterRefusedPush {
    match error {
        SendError::TooLarge => AfterRefusedPush::LeaveForGet,
        _ => AfterRefusedPush::WaitGap,
    }
}

/// Add `transient_id` to the too-large list once, dropping the oldest entry
/// past [`TOO_LARGE_CAP`]. Bounded because the list only ever needs the ids
/// still in the store, and a store that cycles its oldest page out takes
/// the oldest ids with it.
pub fn remember_too_large(list: &mut Vec<TransientId>, transient_id: TransientId) {
    if list.contains(&transient_id) {
        return;
    }
    list.push(transient_id);
    if list.len() > TOO_LARGE_CAP {
        list.remove(0);
    }
}

#[cfg(test)]
#[path = "active_delivery_tests.rs"]
mod tests;
