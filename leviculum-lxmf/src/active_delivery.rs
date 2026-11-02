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
//!
//! The fourth is what a peer that just linked is asked
//! ([`PeerUpSolicit`], [`SolicitPacer`], [`solicit_on_peer_up`],
//! leviculum#849): the push needs a trigger, and a phone that returns to
//! the room gives none of its own.

use alloc::vec::Vec;

use leviculum_core::constants::PATH_REQUEST_MIN_INTERVAL_MS;
use leviculum_core::{Clock, DestinationHash, NodeCore, SendError, Storage, TickOutput};
use rand_core::CryptoRngCore;

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

/// How many stored destinations one peer-up asks about at most.
///
/// The board's store holds mail for a handful of recipients; the bound
/// keeps a full store from turning one link-up into a burst. Past it the
/// destinations whose oldest pushable message is oldest win, so mail that
/// has waited longest is asked about first.
pub const SOLICIT_DESTINATIONS_CAP: usize = 8;

/// How long a (peer, destination) pair stays asked before the same peer is
/// asked again: the core's own pacing for the requests it raises
/// ([`PATH_REQUEST_MIN_INTERVAL_MS`], 20 s).
pub const SOLICIT_INTERVAL_MS: u64 = PATH_REQUEST_MIN_INTERVAL_MS;

/// How many (peer, destination) marks [`SolicitPacer`] holds. Marks past
/// [`SOLICIT_INTERVAL_MS`] are dropped on every call, so only a burst of
/// distinct peers inside one interval can reach the cap; the oldest mark
/// then goes first, and that peer may be asked once more than the interval
/// allows.
pub const SOLICIT_PACER_CAP: usize = 32;

/// The stored destinations a newly linked peer is asked about: every
/// destination with a message an active push could send (the pick
/// [`ActiveCandidate`] would make), at most [`SOLICIT_DESTINATIONS_CAP`].
///
/// A fold over the store's own scan, like [`ActiveCandidate`].
#[derive(Debug, Clone)]
pub struct PeerUpSolicit<'a> {
    excluded: &'a [TransientId],
    /// (destination, sequence of its oldest pushable message).
    picks: Vec<([u8; 16], u64)>,
}

impl<'a> PeerUpSolicit<'a> {
    /// A pick that skips ids in `excluded` (the too-large list: a
    /// destination whose only mail is too large for a push has nothing a
    /// path would trigger).
    pub fn new(excluded: &'a [TransientId]) -> Self {
        Self {
            excluded,
            picks: Vec::new(),
        }
    }

    /// Offer one directory entry.
    pub fn consider(&mut self, meta: &StoredMessage) {
        if self.excluded.contains(&meta.transient_id) {
            return;
        }
        if let Some(pick) = self
            .picks
            .iter_mut()
            .find(|(destination, _)| *destination == meta.destination_hash)
        {
            pick.1 = pick.1.min(meta.sequence);
            return;
        }
        if self.picks.len() < SOLICIT_DESTINATIONS_CAP {
            self.picks.push((meta.destination_hash, meta.sequence));
            return;
        }
        // Full: the newest-waiting destination makes room for an older one.
        if let Some(youngest) = self
            .picks
            .iter_mut()
            .max_by_key(|(_, sequence)| *sequence)
            .filter(|(_, sequence)| meta.sequence < *sequence)
        {
            *youngest = (meta.destination_hash, meta.sequence);
        }
    }

    /// The destinations to ask about, longest-waiting first.
    pub fn destinations(mut self) -> Vec<[u8; 16]> {
        // A stable insertion sort by sequence: the order `sort_by_key` gives
        // for every input, over at most `SOLICIT_DESTINATIONS_CAP` entries,
        // where the standard library sorts by insertion too. Spelled out
        // because the generic stable sort costs about 3 KB of flash on a
        // board for this one element type (order 1300).
        for i in 1..self.picks.len() {
            let mut j = i;
            while j > 0 && self.picks[j - 1].1 > self.picks[j].1 {
                self.picks.swap(j - 1, j);
                j -= 1;
            }
        }
        self.picks
            .into_iter()
            .map(|(destination, _)| destination)
            .collect()
    }
}

/// Which (peer, destination) pairs were asked recently, so a link that
/// flaps does not repeat its requests: each pair is asked at most once per
/// [`SOLICIT_INTERVAL_MS`]. Keyed by the peer as well as the destination,
/// because a second phone linking inside the interval has to be asked too;
/// it may be the one that owns the destination.
#[derive(Debug, Clone, Default)]
pub struct SolicitPacer {
    /// (peer, destination, asked at ms).
    marks: Vec<([u8; 16], [u8; 16], u64)>,
}

impl SolicitPacer {
    /// An empty pacer.
    pub const fn new() -> Self {
        Self { marks: Vec::new() }
    }

    /// Whether `peer` may be asked about `destination` at `now_ms`; a yes
    /// is recorded as asked.
    pub fn due(&mut self, now_ms: u64, peer: [u8; 16], destination: [u8; 16]) -> bool {
        self.marks
            .retain(|(_, _, at_ms)| now_ms.saturating_sub(*at_ms) < SOLICIT_INTERVAL_MS);
        if self
            .marks
            .iter()
            .any(|(p, d, _)| *p == peer && *d == destination)
        {
            return false;
        }
        if self.marks.len() >= SOLICIT_PACER_CAP {
            self.marks.remove(0);
        }
        self.marks.push((peer, destination, now_ms));
        true
    }

    /// The marks held, for the heap census.
    pub fn marks(&self) -> &Vec<([u8; 16], [u8; 16], u64)> {
        &self.marks
    }
}

/// A peer finished its handshake on `iface`: ask it, on its link alone,
/// for every destination in `destinations` the pacer lets through
/// (leviculum#849). Returns what to send and the destinations asked.
///
/// The handshake names the peer's transport identity, and the store is
/// keyed by LXMF delivery destinations, which belong to a different
/// identity on a two-identity client such as Columba, so the node cannot
/// tell which stored destinations the peer owns. A path request is the
/// question every RNS node answers for its own destinations
/// (`reference/Reticulum/RNS/Transport.py:2938-2940`, the local
/// destination arm of `path_request`, which announces as a path response,
/// on the interface the request came in on); the answer is an
/// announce, which triggers the push the same way an unsolicited one
/// does. A peer that owns none of them stays silent.
pub fn solicit_on_peer_up<R, C, S>(
    node: &mut NodeCore<R, C, S>,
    pacer: &mut SolicitPacer,
    iface: usize,
    peer: [u8; 16],
    destinations: &[[u8; 16]],
) -> (TickOutput, Vec<[u8; 16]>)
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let mut out = TickOutput::default();
    let mut asked = Vec::new();
    let now_ms = node.now_ms();
    for destination in destinations {
        if !pacer.due(now_ms, peer, *destination) {
            continue;
        }
        out.merge(node.request_path_to_peer(&DestinationHash::new(*destination), iface, peer));
        asked.push(*destination);
    }
    (out, asked)
}

#[cfg(test)]
#[path = "active_delivery_tests.rs"]
mod tests;
