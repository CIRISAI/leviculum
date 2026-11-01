//! The home node's outbox policy (`docs/src/concepts/loquax-home-node.md`,
//! "Sending through the home node"): a pure state machine, fed the events
//! that matter and answering with what to do, so every rule is tested
//! without a network. The daemon (`super::daemon`) carries it out.
//!
//! 1. Direct delivery at once, over a link, so a peer that is not there
//!    fails crisply after the router's stock attempts.
//! 2. On direct failure, the same packed message goes to the propagation
//!    node, and direct delivery is tried again on every announce of the
//!    recipient and every [`Policy::retry_secs`], for [`Policy::horizon_secs`].
//! 3. DELIVERED when a direct proof arrives, even after the propagation node
//!    took a copy: the recipient keeps one message per hash
//!    (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`), so the copy it later
//!    collects is dropped there.
//!
//! The router holds one entry per message id, so one message is never on
//! two legs at once: a leg starts only when the last one ended.
//!
//! A leg whose end never comes is given up after [`STALL_SECS`] without a
//! sign of it (1293): the daemon calls it off in the router, and the
//! message is treated as a direct failure, handed to the propagation node
//! once and tried direct on the next announce and hour, so no message
//! waits on a leg forever.

use alloc::vec::Vec;

/// How often a message waiting for its recipient is tried direct again,
/// besides on every announce of the recipient: an hour. The announce is the
/// retry that matters, since it says the recipient is there; the cadence
/// covers an announce the home node did not hear, at a cost of one link
/// attempt an hour, 168 over the seven days.
pub const DEFAULT_RETRY_SECS: u64 = 3600;
/// How many days a message is retried direct: seven, as Lew decided on
/// 2026-10-29.
pub const DEFAULT_DAYS: u64 = 7;

/// How long a leg may go without a sign before it is given up: 779 s.
///
/// The longest a direct leg that is still alive goes without one is a link
/// whose peer stopped answering mid-transfer: the core closes it only once
/// it went stale ([`LINK_STALE_TIME_SECS`] and [`LINK_STALE_GRACE_SECS`],
/// 725 s), and the delivery then fails. After that the router's stock
/// attempts ([`MAX_DELIVERY_ATTEMPTS`]) each wait [`DELIVERY_RETRY_WAIT_MS`]
/// before the next, 50 s, before it lets go. The margin is one
/// [`PROCESSING_INTERVAL_MS`], the latest the router looks at the message
/// again. A propagated leg rides a link to the propagation node under the
/// same timeouts. A transfer that is still moving after this is cut too:
/// a message that big over a link that slow is the propagation node's.
///
/// [`LINK_STALE_TIME_SECS`]: leviculum_core::constants::LINK_STALE_TIME_SECS
/// [`LINK_STALE_GRACE_SECS`]: leviculum_core::constants::LINK_STALE_GRACE_SECS
/// [`MAX_DELIVERY_ATTEMPTS`]: leviculum_lxmf::router::MAX_DELIVERY_ATTEMPTS
/// [`DELIVERY_RETRY_WAIT_MS`]: leviculum_lxmf::router::DELIVERY_RETRY_WAIT_MS
/// [`PROCESSING_INTERVAL_MS`]: leviculum_lxmf::router::PROCESSING_INTERVAL_MS
pub const STALL_SECS: u64 = leviculum_core::constants::LINK_STALE_TIME_SECS
    + leviculum_core::constants::LINK_STALE_GRACE_SECS
    + (leviculum_lxmf::router::MAX_DELIVERY_ATTEMPTS as u64
        * leviculum_lxmf::router::DELIVERY_RETRY_WAIT_MS
        + leviculum_lxmf::router::PROCESSING_INTERVAL_MS)
        / 1000;

/// The two ways a message leaves the home node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    /// To the recipient over a link, with a delivery proof.
    Direct,
    /// To the propagation node, for the recipient to collect.
    Propagated,
}

/// How a leg ended, as the router left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A delivery proof came.
    Delivered,
    /// Handed on without a proof: a propagation node took it.
    Sent,
    /// It did not go.
    Failed,
}

/// A state change of an outbox message: each one is a row update and an
/// archive entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A propagation node took a copy; direct retries go on.
    Propagated,
    /// A direct delivery proof came; nothing more is tried.
    Delivered,
    /// The horizon passed without a proof; nothing more is tried.
    /// `propagated` says whether a copy waits on a propagation node.
    Ended { propagated: bool },
    /// A leg went [`STALL_SECS`] without a sign and was given up; the
    /// message is tried again.
    Stalled,
}

impl Change {
    /// The archive kind clients see this change as.
    pub fn kind(self) -> u8 {
        match self {
            Self::Propagated => super::KIND_PROPAGATED,
            Self::Delivered => super::KIND_DELIVERED,
            Self::Ended { propagated: false } => super::KIND_FAILED,
            Self::Ended { propagated: true } => super::KIND_RETRIES_ENDED,
            Self::Stalled => super::KIND_STALLED,
        }
    }

    /// The word the daemon's log uses.
    pub fn word(self) -> &'static str {
        match self {
            Self::Propagated => "propagated",
            Self::Delivered => "delivered",
            Self::Ended { propagated: false } => "failed",
            Self::Ended { propagated: true } => "retries-ended",
            Self::Stalled => "stalled",
        }
    }
}

/// What the daemon is to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Hand the message to the router on `leg`.
    Send { id: [u8; 32], leg: Leg },
    /// Record a state change: the outbox row and an archive entry.
    Record { id: [u8; 32], change: Change },
    /// Call off `leg` of `id` in the router: it went [`Policy::stall_secs`]
    /// without a sign, and whatever the router still holds of it would
    /// refuse the next leg as a duplicate.
    GiveUp { id: [u8; 32], leg: Leg },
}

/// The timing, from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The slow cadence of direct retries, in seconds.
    pub retry_secs: i64,
    /// How long after it was handed in a message is retried, in seconds.
    pub horizon_secs: i64,
    /// How long a leg may go without a sign before it is given up, in
    /// seconds ([`STALL_SECS`]).
    pub stall_secs: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            retry_secs: DEFAULT_RETRY_SECS as i64,
            horizon_secs: (DEFAULT_DAYS * 24 * 3600) as i64,
            stall_secs: STALL_SECS as i64,
        }
    }
}

struct Item {
    id: [u8; 32],
    destination: [u8; 16],
    packed: Vec<u8>,
    created_at: i64,
    propagated: bool,
    in_flight: Option<Leg>,
    /// When the leg in flight started or last gave a sign.
    heard_at: i64,
    /// A direct leg was given up and the propagation node is owed the
    /// copy a failure would have sent it.
    propagation_owed: bool,
    next_direct_at: i64,
}

impl Item {
    fn start(&mut self, leg: Leg, now: i64) -> Act {
        self.in_flight = Some(leg);
        self.heard_at = now;
        if leg == Leg::Direct {
            // A direct failure propagates by itself.
            self.propagation_owed = false;
        }
        Act::Send { id: self.id, leg }
    }
}

/// The messages still retried, in the order they came.
pub struct Outgoing {
    policy: Policy,
    items: Vec<Item>,
}

impl Outgoing {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            items: Vec::new(),
        }
    }

    /// Take one message in: a new one from `/send`, or one the store kept
    /// across a restart. Either is due direct at the next [`Outgoing::tick`].
    pub fn add(
        &mut self,
        id: [u8; 32],
        destination: [u8; 16],
        packed: Vec<u8>,
        created_at: i64,
        propagated: bool,
    ) {
        if self.items.iter().any(|item| item.id == id) {
            return;
        }
        self.items.push(Item {
            id,
            destination,
            packed,
            created_at,
            propagated,
            in_flight: None,
            heard_at: i64::MIN,
            propagation_owed: false,
            next_direct_at: i64::MIN,
        });
    }

    /// The packed message, while it is retried.
    pub fn packed(&self, id: &[u8; 32]) -> Option<&[u8]> {
        self.item(id).map(|item| item.packed.as_slice())
    }

    /// The leg `id` is on now.
    pub fn in_flight(&self, id: &[u8; 32]) -> Option<Leg> {
        self.item(id).and_then(|item| item.in_flight)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn item(&self, id: &[u8; 32]) -> Option<&Item> {
        self.items.iter().find(|item| item.id == *id)
    }

    /// The router said something of `id`'s leg: it is alive, and the stall
    /// bound counts from now.
    pub fn heard(&mut self, id: &[u8; 32], now: i64) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == *id) {
            if item.in_flight.is_some() {
                item.heard_at = now;
            }
        }
    }

    /// The clock moved: give up a leg that went silent, end what passed its
    /// horizon, and try what is due.
    pub fn tick(&mut self, now: i64) -> Vec<Act> {
        let mut acts = Vec::new();
        let horizon = self.policy.horizon_secs;
        let retry = self.policy.retry_secs;
        let stall = self.policy.stall_secs;
        self.items.retain_mut(|item| {
            if let Some(leg) = item.in_flight {
                if now.saturating_sub(item.heard_at) < stall {
                    return true;
                }
                item.in_flight = None;
                item.propagation_owed |= leg == Leg::Direct && !item.propagated;
                acts.push(Act::GiveUp { id: item.id, leg });
                acts.push(Act::Record {
                    id: item.id,
                    change: Change::Stalled,
                });
                // The next leg waits for the next tick, so the end the
                // router reports for the leg called off comes back first
                // and finds nothing on a leg.
                return true;
            }
            if now.saturating_sub(item.created_at) >= horizon {
                acts.push(Act::Record {
                    id: item.id,
                    change: Change::Ended {
                        propagated: item.propagated,
                    },
                });
                return false;
            }
            if item.propagation_owed {
                item.propagation_owed = false;
                acts.push(item.start(Leg::Propagated, now));
            } else if now >= item.next_direct_at {
                item.next_direct_at = now.saturating_add(retry);
                acts.push(item.start(Leg::Direct, now));
            }
            true
        });
        acts
    }

    /// `destination` announced: it is there now, so every message for it
    /// that is not on a leg goes direct at once.
    pub fn announced(&mut self, destination: [u8; 16], now: i64) -> Vec<Act> {
        let horizon = self.policy.horizon_secs;
        let retry = self.policy.retry_secs;
        let mut acts = Vec::new();
        for item in &mut self.items {
            if item.destination != destination
                || item.in_flight.is_some()
                || now.saturating_sub(item.created_at) >= horizon
            {
                continue;
            }
            item.next_direct_at = now.saturating_add(retry);
            acts.push(item.start(Leg::Direct, now));
        }
        acts
    }

    /// The router let go of `id` with `outcome` at `now`. A message not on
    /// a leg is not this outbox's, and changes nothing.
    pub fn left(&mut self, id: &[u8; 32], outcome: Outcome, now: i64) -> Vec<Act> {
        let Some(index) = self.items.iter().position(|item| item.id == *id) else {
            return Vec::new();
        };
        let item = &mut self.items[index];
        let Some(leg) = item.in_flight.take() else {
            return Vec::new();
        };
        match (leg, outcome) {
            (Leg::Direct, Outcome::Delivered) => {
                self.items.remove(index);
                alloc::vec![Act::Record {
                    id: *id,
                    change: Change::Delivered,
                }]
            }
            // A direct leg without a proof failed, whatever the router
            // called its last state: only a proof says the peer has it.
            (Leg::Direct, _) if !item.propagated => alloc::vec![item.start(Leg::Propagated, now)],
            (Leg::Direct, _) => Vec::new(),
            (Leg::Propagated, Outcome::Sent | Outcome::Delivered) => {
                item.propagated = true;
                alloc::vec![Act::Record {
                    id: *id,
                    change: Change::Propagated,
                }]
            }
            // No node took it; the next direct failure tries again.
            (Leg::Propagated, Outcome::Failed) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: [u8; 32] = [1; 32];
    const PEER: [u8; 16] = [2; 16];
    const DAY: i64 = 24 * 3600;

    fn outbox() -> Outgoing {
        let mut outgoing = Outgoing::new(Policy::default());
        outgoing.add(ID, PEER, alloc::vec![9; 40], 1000, false);
        outgoing
    }

    fn send(leg: Leg) -> Act {
        Act::Send { id: ID, leg }
    }

    fn record(change: Change) -> Act {
        Act::Record { id: ID, change }
    }

    #[test]
    fn direct_goes_at_once_and_a_proof_ends_it() {
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        // On a leg, nothing more is started, not even by an announce.
        assert!(outgoing.tick(1001).is_empty());
        assert!(outgoing.announced(PEER, 1002).is_empty());
        assert_eq!(
            outgoing.left(&ID, Outcome::Delivered, 0),
            [record(Change::Delivered)]
        );
        assert!(outgoing.is_empty());
        assert!(outgoing.tick(1000 + 30 * DAY).is_empty());
    }

    #[test]
    fn a_failed_direct_leg_propagates_then_a_later_proof_delivers() {
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Failed, 0),
            [send(Leg::Propagated)]
        );
        assert_eq!(outgoing.in_flight(&ID), Some(Leg::Propagated));
        assert_eq!(
            outgoing.left(&ID, Outcome::Sent, 0),
            [record(Change::Propagated)]
        );
        // Nothing until the recipient announces or the cadence comes round.
        assert!(outgoing.tick(1100).is_empty());
        assert_eq!(outgoing.announced([7; 16], 1200), []);
        assert_eq!(outgoing.announced(PEER, 1200), [send(Leg::Direct)]);
        // A second failure does not propagate again: the node has a copy.
        assert!(outgoing.left(&ID, Outcome::Failed, 0).is_empty());
        // The cadence counts from the last direct attempt.
        assert!(outgoing.tick(1200 + 3599).is_empty());
        assert_eq!(outgoing.tick(1200 + 3600), [send(Leg::Direct)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Delivered, 0),
            [record(Change::Delivered)]
        );
        assert!(outgoing.is_empty());
    }

    #[test]
    fn a_propagation_no_node_took_is_tried_again_after_the_next_direct_failure() {
        let mut outgoing = outbox();
        outgoing.tick(1000);
        outgoing.left(&ID, Outcome::Failed, 0);
        assert!(outgoing.left(&ID, Outcome::Failed, 0).is_empty());
        assert_eq!(outgoing.tick(1000 + 3600), [send(Leg::Direct)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Failed, 0),
            [send(Leg::Propagated)]
        );
    }

    #[test]
    fn the_seven_day_horizon_ends_the_retries() {
        let mut outgoing = outbox();
        outgoing.tick(1000);
        outgoing.left(&ID, Outcome::Failed, 0);
        outgoing.left(&ID, Outcome::Sent, 0);
        assert!(outgoing.tick(1000 + 7 * DAY - 1).len() == 1);
        outgoing.left(&ID, Outcome::Failed, 0);
        // An announce after the horizon starts nothing; the tick ends it.
        assert!(outgoing.announced(PEER, 1000 + 7 * DAY).is_empty());
        assert_eq!(
            outgoing.tick(1000 + 7 * DAY),
            [record(Change::Ended { propagated: true })]
        );
        assert!(outgoing.is_empty());
    }

    #[test]
    fn a_message_no_node_ever_took_ends_as_failed() {
        let mut outgoing = Outgoing::new(Policy {
            retry_secs: 10,
            horizon_secs: 100,
            stall_secs: 1000,
        });
        outgoing.add(ID, PEER, Vec::new(), 0, false);
        let mut now = 0;
        while !outgoing.is_empty() {
            for act in outgoing.tick(now) {
                if let Act::Record { change, .. } = act {
                    assert_eq!(change, Change::Ended { propagated: false });
                    assert_eq!(change.kind(), crate::home::KIND_FAILED);
                }
            }
            while outgoing.in_flight(&ID).is_some() {
                outgoing.left(&ID, Outcome::Failed, 0);
            }
            now += 5;
            assert!(now <= 200, "the horizon never ended it");
        }
        assert_eq!(now, 105);
    }

    #[test]
    fn a_leg_running_at_the_horizon_finishes_first() {
        let mut outgoing = Outgoing::new(Policy {
            retry_secs: 10,
            horizon_secs: 100,
            stall_secs: 1000,
        });
        outgoing.add(ID, PEER, Vec::new(), 0, true);
        assert_eq!(outgoing.tick(95), [send(Leg::Direct)]);
        assert!(outgoing.tick(150).is_empty());
        assert_eq!(
            outgoing.left(&ID, Outcome::Delivered, 0),
            [record(Change::Delivered)]
        );
    }

    #[test]
    fn a_restored_message_is_due_at_once_and_keeps_its_propagated_copy() {
        let mut outgoing = Outgoing::new(Policy::default());
        outgoing.add(ID, PEER, Vec::new(), 1000, true);
        assert_eq!(outgoing.tick(5000), [send(Leg::Direct)]);
        assert!(outgoing.left(&ID, Outcome::Failed, 0).is_empty());
    }

    /// 1293: a leg whose end never comes (a link that died mid-leg, a
    /// stalled transfer) is given up after the bound: called off in the
    /// router, recorded, handed to the propagation node once on the next
    /// tick, and tried direct again on the next announce.
    #[test]
    fn a_direct_leg_that_never_ends_is_given_up_and_handed_on() {
        assert_eq!(STALL_SECS, 779);
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        assert!(outgoing.tick(1000 + 778).is_empty());
        assert!(outgoing.announced(PEER, 1000 + 778).is_empty());
        assert_eq!(
            outgoing.tick(1000 + 779),
            [
                Act::GiveUp {
                    id: ID,
                    leg: Leg::Direct
                },
                record(Change::Stalled)
            ]
        );
        assert_eq!(outgoing.in_flight(&ID), None);
        // The router's end of the leg called off finds nothing on a leg.
        assert!(outgoing.left(&ID, Outcome::Failed, 1000 + 779).is_empty());
        assert_eq!(outgoing.tick(1000 + 780), [send(Leg::Propagated)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Sent, 1000 + 790),
            [record(Change::Propagated)]
        );
        // Once only: the next direct leg is the announce's, and its
        // failure sends no second copy.
        assert!(outgoing.tick(1000 + 800).is_empty());
        assert_eq!(outgoing.announced(PEER, 1000 + 900), [send(Leg::Direct)]);
        assert!(outgoing.left(&ID, Outcome::Failed, 1000 + 910).is_empty());
    }

    /// A propagated leg that never ends is given up as well, and the
    /// direct retries go on at the hour; the next direct failure offers the
    /// propagation node the copy again, as after a node that took nothing.
    #[test]
    fn a_propagated_leg_that_never_ends_is_given_up_and_the_hour_tries_direct() {
        let mut outgoing = outbox();
        outgoing.tick(1000);
        assert_eq!(
            outgoing.left(&ID, Outcome::Failed, 1010),
            [send(Leg::Propagated)]
        );
        assert!(outgoing.tick(1010 + 778).is_empty());
        assert_eq!(
            outgoing.tick(1010 + 779),
            [
                Act::GiveUp {
                    id: ID,
                    leg: Leg::Propagated
                },
                record(Change::Stalled)
            ]
        );
        assert!(outgoing.tick(1010 + 780).is_empty());
        assert_eq!(outgoing.tick(1000 + 3600), [send(Leg::Direct)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Failed, 1000 + 3610),
            [send(Leg::Propagated)]
        );
    }

    /// A sign of the leg (a state the router reported) moves the bound:
    /// a slow transfer that is still talking is not cut.
    #[test]
    fn a_leg_that_gives_a_sign_is_not_given_up() {
        let mut outgoing = outbox();
        outgoing.tick(1000);
        outgoing.heard(&ID, 1500);
        assert!(outgoing.tick(1000 + 779).is_empty());
        assert!(outgoing.tick(1500 + 778).is_empty());
        assert_eq!(outgoing.tick(1500 + 779).len(), 2);
    }

    /// A restart finds a message that was on a leg in a process that no
    /// longer exists: the store keeps no leg, so it is due direct at once
    /// and handed on like any other when that fails.
    #[test]
    fn a_message_on_a_leg_at_a_restart_is_due_at_once() {
        let mut outgoing = Outgoing::new(Policy::default());
        outgoing.add(ID, PEER, Vec::new(), 1000, false);
        assert_eq!(outgoing.tick(90_000), [send(Leg::Direct)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Failed, 90_010),
            [send(Leg::Propagated)]
        );
    }

    #[test]
    fn what_is_not_on_a_leg_is_not_ours() {
        let mut outgoing = outbox();
        assert!(outgoing.left(&ID, Outcome::Delivered, 0).is_empty());
        assert!(outgoing.left(&[5; 32], Outcome::Delivered, 0).is_empty());
        assert_eq!(outgoing.len(), 1);
        // The same message twice is one.
        outgoing.add(ID, PEER, Vec::new(), 1000, false);
        assert_eq!(outgoing.len(), 1);
    }
}
