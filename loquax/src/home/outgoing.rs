//! The home node's outbox policy (`docs/src/concepts/loquax-home-node.md`,
//! "Sending through the home node"): a pure state machine, fed the events
//! that matter and answering with what to do, so every rule is tested
//! without a network. The daemon (`super::daemon`) carries it out.
//!
//! 1. Direct delivery at once, over a link, so a peer that is not there
//!    fails crisply after the router's stock attempts.
//! 2. On direct failure, the same packed message goes to the propagation
//!    node, and direct delivery is tried again on every announce of the
//!    recipient and every [`Policy::retry_secs`], doubled for each direct
//!    leg in a row that went unproven up to [`MAX_RETRY_DOUBLINGS`] times,
//!    for [`Policy::horizon_secs`].
//! 3. DELIVERED when a direct proof arrives, even after the propagation node
//!    took a copy: the recipient keeps one message per hash
//!    (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`), so the copy it later
//!    collects is dropped there.
//!
//! The router holds one entry per message id, so one message is never on
//! two legs at once: a leg starts only when the last one ended.
//!
//! A direct leg the recipient never answers (no link, so the message never
//! reached `Sending`) is given up after [`ANSWER_SECS`] (1328) and handed
//! to the propagation node, rather than waiting out the router's stock
//! attempts: the announce and hourly retries go on regardless.
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
/// attempt an hour at first; the gap doubles for a recipient that keeps
/// not answering ([`MAX_RETRY_DOUBLINGS`]).
pub const DEFAULT_RETRY_SECS: u64 = 3600;
/// How often the slow cadence doubles for a recipient that keeps not
/// answering: three times, so the gap grows 1 h, 2 h, 4 h and stays at
/// 8 h (order 1585).
///
/// The hourly cadence tried every waiting message direct 24 times a day
/// whoever answered: on miauhaus 22 messages for peers that were away made
/// 521 `direct unanswered` lines a day, each a link attempt, a path request
/// and a log line. With the doubling a message waiting the whole seven
/// days goes direct about 24 times instead of 168. The recipient's own
/// announce still tries it at once, so a recipient that comes back is
/// reached as soon as before.
pub const MAX_RETRY_DOUBLINGS: u32 = 3;

/// How many days a message is retried direct: seven, as Lew decided on
/// 2026-10-29.
pub const DEFAULT_DAYS: u64 = 7;

/// How long a direct leg may wait for the recipient to answer, its message
/// never reaching `Sending`, before it is given up and the propagation
/// node is offered the copy: 45 s (1328, 1331).
///
/// The router's stock direct leg against a recipient that does not answer
/// runs [`MAX_DELIVERY_ATTEMPTS`] link attempts, each of which the core
/// retries twice with fresh keys and follows with a path request: on a
/// 2-hop path over TCP one attempt took 40 s in the reproducer
/// (`home_node.rs`, `..._reaches_a_costly_local_node`), the leg about six
/// minutes, and on miauhaus legs ran past [`STALL_SECS`] without ending
/// (1328), so a message to a Columba user who was offline waited thirteen
/// minutes for its propagated copy. The reference's single link
/// establishment waits 6 s per hop plus the first hop's time
/// (`reference/Reticulum/RNS/Link.py:283-284`, `Reticulum.py:142`), so a
/// peer that is there answers well inside the first attempt; 45 s covers
/// that first attempt with its retries over two hops and the margin to
/// start sending. What the bound cuts off costs nothing that matters: the
/// recipient keeps one message per hash, and the next announce of the
/// recipient tries direct again at once.
///
/// It sits below the router's own end of such a leg (1331). A recipient
/// whose key the home never held fails in the router after
/// [`MAX_DELIVERY_ATTEMPTS`] attempts of [`DELIVERY_RETRY_WAIT_MS`] each,
/// which its [`PROCESSING_INTERVAL_MS`] passes round up to 12 s: 60 s,
/// and never under 50 s whatever the passes' phase
/// (`leviculum-lxmf/src/router/direct_retry_tests.rs`). The outbox checks
/// this bound on a 1 s tick against a start in whole seconds, so it fires
/// between 44 s and 47 s after the leg began: 3 s under the router's
/// floor, 13 s under what it does. At 60 s the two met within a second and
/// the phase of the daemon's start chose which one ended the leg. A router
/// that ends an unanswered leg first anyway (paths answered at once, a
/// refusal) is the same fact and is told as `unanswered` too.
///
/// [`MAX_DELIVERY_ATTEMPTS`]: leviculum_lxmf::router::MAX_DELIVERY_ATTEMPTS
/// [`DELIVERY_RETRY_WAIT_MS`]: leviculum_lxmf::router::DELIVERY_RETRY_WAIT_MS
/// [`PROCESSING_INTERVAL_MS`]: leviculum_lxmf::router::PROCESSING_INTERVAL_MS
pub const ANSWER_SECS: u64 = 45;

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
    /// A propagated leg that did not go since the recipient's key is not
    /// held (1406): the node's copy is encrypted for the recipient. The
    /// recipient's path was asked, and the answer brings the key.
    NoKey,
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
    /// Call off the direct leg of `id` in the router: the recipient did not
    /// answer within [`Policy::answer_secs`]. A direct failure, recorded as
    /// none. `offer` says whether the propagation node is offered the copy
    /// next: not when a node took it already (1406: the line said it was
    /// for every message, 22 of 27 on the field mailbox were on the node).
    Unanswered { id: [u8; 32], offer: bool },
}

/// How the recipient was heard, as the router said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Its own announce, live: it is there now.
    Announce,
    /// An answer to a path request, as a rule from a transport node's
    /// cache: it announced once, which says nothing of now (1330).
    PathResponse,
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
    /// How long a direct leg may wait for the recipient to answer, in
    /// seconds ([`ANSWER_SECS`]).
    pub answer_secs: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            retry_secs: DEFAULT_RETRY_SECS as i64,
            horizon_secs: (DEFAULT_DAYS * 24 * 3600) as i64,
            stall_secs: STALL_SECS as i64,
            answer_secs: ANSWER_SECS as i64,
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
    /// When the leg in flight started.
    started_at: i64,
    /// The recipient answered the direct leg in flight: its message
    /// reached `Sending`.
    answered: bool,
    /// A direct leg was given up and the propagation node is owed the
    /// copy a failure would have sent it.
    propagation_owed: bool,
    /// The last propagated leg did not go for want of the recipient's key
    /// ([`Outcome::NoKey`]): the recipient's path response, which brings
    /// the key, offers the node the copy.
    key_wanted: bool,
    next_direct_at: i64,
    /// Direct legs in a row that ended without a proof: each doubles the
    /// slow cadence, up to [`MAX_RETRY_DOUBLINGS`].
    misses: u32,
}

impl Item {
    fn start(&mut self, leg: Leg, now: i64) -> Act {
        self.in_flight = Some(leg);
        self.heard_at = now;
        self.started_at = now;
        self.answered = false;
        // A direct failure propagates by itself; a propagated leg pays
        // what was owed, whatever started it.
        self.propagation_owed = false;
        if leg == Leg::Propagated {
            self.key_wanted = false;
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
            started_at: i64::MIN,
            answered: false,
            propagation_owed: false,
            key_wanted: false,
            next_direct_at: i64::MIN,
            misses: 0,
        });
    }

    /// The packed message, while it is retried.
    pub fn packed(&self, id: &[u8; 32]) -> Option<&[u8]> {
        self.item(id).map(|item| item.packed.as_slice())
    }

    /// Direct legs of `id` in a row that went unproven.
    pub fn misses(&self, id: &[u8; 32]) -> u32 {
        self.item(id).map_or(0, |item| item.misses)
    }

    /// The recipient of `id`, while it is retried.
    pub fn destination(&self, id: &[u8; 32]) -> Option<[u8; 16]> {
        self.item(id).map(|item| item.destination)
    }

    /// Whether a propagation node took a copy of `id`.
    pub fn propagated(&self, id: &[u8; 32]) -> bool {
        self.item(id).is_some_and(|item| item.propagated)
    }

    /// The leg `id` is on now.
    pub fn in_flight(&self, id: &[u8; 32]) -> Option<Leg> {
        self.item(id).and_then(|item| item.in_flight)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The messages held and their heap bytes, each packed message's
    /// included, for the mailbox's census (order 1585).
    pub fn heap_bytes(&self) -> usize {
        self.items.capacity() * core::mem::size_of::<Item>()
            + self
                .items
                .iter()
                .map(|item| item.packed.capacity())
                .sum::<usize>()
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

    /// `id` is on a direct leg the recipient has not answered: its message
    /// never reached `Sending`. The router ending such a leg says the same
    /// as [`Act::Unanswered`]: nobody answered (1331).
    pub fn unanswered(&self, id: &[u8; 32]) -> bool {
        self.item(id)
            .is_some_and(|item| item.in_flight == Some(Leg::Direct) && !item.answered)
    }

    /// The router reported `id`'s message sending: the recipient answered
    /// the leg, which is a sign of it too.
    pub fn answered(&mut self, id: &[u8; 32], now: i64) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == *id) {
            if item.in_flight.is_some() {
                item.heard_at = now;
                item.answered = true;
            }
        }
    }

    /// The clock moved: give up a direct leg nobody answered and a leg that
    /// went silent, end what passed its horizon, and try what is due.
    pub fn tick(&mut self, now: i64) -> Vec<Act> {
        let mut acts = Vec::new();
        let horizon = self.policy.horizon_secs;
        let retry = self.policy.retry_secs;
        let stall = self.policy.stall_secs;
        let answer = self.policy.answer_secs;
        self.items.retain_mut(|item| {
            if item.in_flight == Some(Leg::Direct)
                && !item.answered
                && now.saturating_sub(item.started_at) >= answer
            {
                item.in_flight = None;
                item.propagation_owed |= !item.propagated;
                item.misses = item.misses.saturating_add(1);
                acts.push(Act::Unanswered {
                    id: item.id,
                    offer: !item.propagated,
                });
                // The next leg waits for the next tick, as after a stall.
                return true;
            }
            if let Some(leg) = item.in_flight {
                if now.saturating_sub(item.heard_at) < stall {
                    return true;
                }
                item.in_flight = None;
                item.propagation_owed |= leg == Leg::Direct && !item.propagated;
                if leg == Leg::Direct {
                    item.misses = item.misses.saturating_add(1);
                }
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
                item.next_direct_at = now.saturating_add(backoff(retry, item.misses));
                acts.push(item.start(Leg::Direct, now));
            }
            true
        });
        acts
    }

    /// `destination` announced: it is there now, so every message for it
    /// that is not on a leg goes direct at once.
    ///
    /// A path response starts no direct leg (1330): every direct leg given
    /// up or failed ends in a path request of the router's own, which a
    /// transport node answers from its cache within a minute, and taking
    /// that answer for the recipient tried an offline member direct every
    /// 100 s after its copy was handed on, where the announce and the hour
    /// were meant. It does bring the recipient's key, so a message whose
    /// propagated leg wanted it is offered to the node again, once per
    /// [`Outcome::NoKey`] (1406).
    pub fn announced(&mut self, destination: [u8; 16], heard: Heard, now: i64) -> Vec<Act> {
        let horizon = self.policy.horizon_secs;
        if heard == Heard::PathResponse {
            return self
                .items
                .iter_mut()
                .filter(|item| {
                    item.destination == destination
                        && item.key_wanted
                        && item.in_flight.is_none()
                        && now.saturating_sub(item.created_at) < horizon
                })
                .map(|item| item.start(Leg::Propagated, now))
                .collect();
        }
        let retry = self.policy.retry_secs;
        let mut acts = Vec::new();
        for item in &mut self.items {
            // A copy the propagation node is owed goes first, at the next
            // tick (1328).
            if item.destination != destination
                || item.in_flight.is_some()
                || item.propagation_owed
                || now.saturating_sub(item.created_at) >= horizon
            {
                continue;
            }
            item.next_direct_at = now.saturating_add(backoff(retry, item.misses));
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
            (Leg::Direct, _) if !item.propagated => {
                item.misses = item.misses.saturating_add(1);
                alloc::vec![item.start(Leg::Propagated, now)]
            }
            (Leg::Direct, _) => {
                item.misses = item.misses.saturating_add(1);
                Vec::new()
            }
            (Leg::Propagated, Outcome::Sent | Outcome::Delivered) => {
                item.propagated = true;
                alloc::vec![Act::Record {
                    id: *id,
                    change: Change::Propagated,
                }]
            }
            // No node took it; the next direct failure tries again.
            (Leg::Propagated, Outcome::Failed) => Vec::new(),
            // The same, and the recipient's path response tries again too.
            (Leg::Propagated, Outcome::NoKey) => {
                item.key_wanted = true;
                Vec::new()
            }
        }
    }
}

/// The slow cadence after `misses` direct legs in a row without a proof:
/// `retry` doubled once a miss, at most [`MAX_RETRY_DOUBLINGS`] times.
pub fn backoff(retry: i64, misses: u32) -> i64 {
    retry.saturating_mul(1 << misses.min(MAX_RETRY_DOUBLINGS))
}

/// How often a peer's outbox line may be said: once an hour (order 1585).
pub const PEER_LINE_EVERY_SECS: i64 = 3600;

/// Peers whose line was said lately, at most this many: an older one
/// makes room, and the worst it costs is one line too many.
pub const PEER_LINES_CAP: usize = 1024;

/// The outbox's lines about a recipient that does not answer, said once a
/// peer an hour and not once an attempt (order 1585): the field mailbox
/// said `direct unanswered` 521 times a day for a few peers that were away.
/// The line that is said counts the ones held back since.
#[derive(Default)]
pub struct PeerLines {
    said: alloc::collections::BTreeMap<[u8; 16], (i64, u32)>,
}

impl PeerLines {
    /// May a line about `peer` be said at `now`? `Some(held)` with the
    /// lines held back since the last one, `None` while it is quiet.
    pub fn admit(&mut self, peer: [u8; 16], now: i64) -> Option<u32> {
        if let Some((at, held)) = self.said.get_mut(&peer) {
            if now.saturating_sub(*at) < PEER_LINE_EVERY_SECS {
                *held = held.saturating_add(1);
                return None;
            }
            let before = *held;
            *at = now;
            *held = 0;
            return Some(before);
        }
        if self.said.len() >= PEER_LINES_CAP {
            self.said
                .retain(|_, (at, _)| now.saturating_sub(*at) < PEER_LINE_EVERY_SECS);
            if self.said.len() >= PEER_LINES_CAP {
                if let Some(oldest) = self
                    .said
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(peer, _)| *peer)
                {
                    self.said.remove(&oldest);
                }
            }
        }
        self.said.insert(peer, (now, 0));
        Some(0)
    }

    /// Peers held and their estimated heap bytes, for the census.
    pub fn census(&self) -> (usize, usize) {
        (
            self.said.len(),
            leviculum_core::heap_census::btree_map_bytes(&self.said),
        )
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

    fn unanswered(offer: bool) -> Act {
        Act::Unanswered { id: ID, offer }
    }

    /// Order 1585: a recipient that never answers is tried direct at a
    /// doubling gap, 1 h, 2 h, 4 h, then every 8 h; its announce still
    /// tries at once.
    #[test]
    fn the_slow_cadence_doubles_for_a_recipient_that_never_answers() {
        let mut outgoing = outbox();
        let mut starts = Vec::new();
        let mut now = 1000;
        while now < 1000 + 2 * DAY {
            for act in outgoing.tick(now) {
                match act {
                    Act::Send {
                        leg: Leg::Direct, ..
                    } => starts.push(now),
                    // The propagated copy goes and is taken.
                    Act::Send {
                        leg: Leg::Propagated,
                        ..
                    } => {
                        outgoing.left(&ID, Outcome::Sent, now);
                    }
                    _ => {}
                }
            }
            now += 1;
        }
        let gaps: Vec<i64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(&gaps[..4], [3600, 7200, 14400, 28800], "{gaps:?}");
        assert!(gaps[4..].iter().all(|gap| *gap == 28800), "{gaps:?}");
        // Nine in two days (0, 1, 3, 7, 15, 23, 31, 39, 47 h); about 24
        // over the seven days, where the hour made 168.
        assert_eq!(starts.len(), 9, "{starts:?}");
        // The recipient's announce tries at once, whatever the gap.
        assert_eq!(
            outgoing.announced(PEER, Heard::Announce, now),
            [send(Leg::Direct)]
        );
    }

    #[test]
    fn a_peers_line_is_said_once_an_hour_with_the_count_held_back() {
        let mut lines = PeerLines::default();
        assert_eq!(lines.admit(PEER, 0), Some(0));
        for t in 1..10 {
            assert_eq!(lines.admit(PEER, t * 60), None);
        }
        // Another peer is its own.
        assert_eq!(lines.admit([3; 16], 100), Some(0));
        assert_eq!(lines.admit(PEER, 3600), Some(9));
        assert_eq!(lines.admit(PEER, 3601), None);
        // Held to its cap.
        for n in 0..PEER_LINES_CAP as u32 + 5 {
            let mut peer = [0; 16];
            peer[..4].copy_from_slice(&n.to_be_bytes());
            lines.admit(peer, 4000);
        }
        assert!(lines.census().0 <= PEER_LINES_CAP);
    }

    #[test]
    fn direct_goes_at_once_and_a_proof_ends_it() {
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        // On a leg, nothing more is started, not even by an announce.
        assert!(outgoing.tick(1001).is_empty());
        assert!(outgoing.announced(PEER, Heard::Announce, 1002).is_empty());
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
        assert_eq!(outgoing.announced([7; 16], Heard::Announce, 1200), []);
        assert_eq!(
            outgoing.announced(PEER, Heard::Announce, 1200),
            [send(Leg::Direct)]
        );
        // A second failure does not propagate again: the node has a copy.
        assert!(outgoing.left(&ID, Outcome::Failed, 0).is_empty());
        // The cadence counts from the last direct attempt, doubled for the
        // one miss before it (1585).
        assert!(outgoing.tick(1200 + 7199).is_empty());
        assert_eq!(outgoing.tick(1200 + 7200), [send(Leg::Direct)]);
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
        assert!(outgoing
            .announced(PEER, Heard::Announce, 1000 + 7 * DAY)
            .is_empty());
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
            answer_secs: 1000,
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
            answer_secs: 1000,
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
        // The recipient answered, then the link died mid-leg.
        outgoing.answered(&ID, 1000);
        assert!(outgoing.tick(1000 + 778).is_empty());
        assert!(outgoing
            .announced(PEER, Heard::Announce, 1000 + 778)
            .is_empty());
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
        assert_eq!(
            outgoing.announced(PEER, Heard::Announce, 1000 + 900),
            [send(Leg::Direct)]
        );
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
        outgoing.answered(&ID, 1001);
        outgoing.heard(&ID, 1500);
        assert!(outgoing.tick(1000 + 779).is_empty());
        assert!(outgoing.tick(1500 + 778).is_empty());
        assert_eq!(outgoing.tick(1500 + 779).len(), 2);
    }

    /// 1328: a direct leg the recipient does not answer within the bound
    /// (an announced Columba user who went offline) is called off, records
    /// nothing, and the next tick offers the propagation node the copy;
    /// the announce still tries direct. Controls: an answered leg runs on
    /// past the bound, and a message already on a node is not offered
    /// again.
    #[test]
    fn a_direct_leg_nobody_answers_goes_to_the_node_within_the_bound() {
        assert_eq!(ANSWER_SECS, 45);
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        // A sign that is no answer (a state before sending) moves nothing.
        outgoing.heard(&ID, 1030);
        assert!(outgoing.unanswered(&ID));
        assert!(outgoing.tick(1044).is_empty());
        assert_eq!(outgoing.tick(1045), [unanswered(true)]);
        assert_eq!(outgoing.in_flight(&ID), None);
        assert!(outgoing.left(&ID, Outcome::Failed, 1045).is_empty());
        // Neither the path response the router's request drew nor an
        // announce starts a direct leg before the owed copy.
        assert!(outgoing
            .announced(PEER, Heard::PathResponse, 1045)
            .is_empty());
        assert!(outgoing.announced(PEER, Heard::Announce, 1045).is_empty());
        assert_eq!(outgoing.tick(1061), [send(Leg::Propagated)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Sent, 1062),
            [record(Change::Propagated)]
        );
        // Already on the node: the announce's unanswered leg ends with no
        // second copy.
        assert_eq!(
            outgoing.announced(PEER, Heard::Announce, 1100),
            [send(Leg::Direct)]
        );
        assert_eq!(outgoing.tick(1145), [unanswered(false)]);
        assert!(outgoing.tick(1146).is_empty());
        // Control: answered, the leg is not cut at the bound.
        assert_eq!(
            outgoing.announced(PEER, Heard::Announce, 1200),
            [send(Leg::Direct)]
        );
        outgoing.answered(&ID, 1205);
        assert!(!outgoing.unanswered(&ID));
        assert!(outgoing.tick(1300).is_empty());
        assert_eq!(outgoing.in_flight(&ID), Some(Leg::Direct));
    }

    /// 1331: the home's answer bound and the router's own end of an
    /// unanswered direct leg race, and either order offers the propagation
    /// node the copy exactly once. The router's 60 s for a recipient whose
    /// key the home never held is pinned in
    /// `leviculum-lxmf/src/router/direct_retry_tests.rs`; the bound sits
    /// under it with the tick phase and the whole-second start included.
    #[test]
    fn either_end_of_an_unanswered_direct_leg_offers_the_node_once() {
        use leviculum_lxmf::router::{
            DELIVERY_RETRY_WAIT_MS, MAX_DELIVERY_ATTEMPTS, PROCESSING_INTERVAL_MS,
        };
        // The router's floor: every attempt waits the retry wait at least.
        let router_floor_secs = u64::from(MAX_DELIVERY_ATTEMPTS) * DELIVERY_RETRY_WAIT_MS / 1000;
        let router_secs = u64::from(MAX_DELIVERY_ATTEMPTS)
            * DELIVERY_RETRY_WAIT_MS.div_ceil(PROCESSING_INTERVAL_MS)
            * PROCESSING_INTERVAL_MS
            / 1000;
        assert_eq!((router_floor_secs, router_secs), (50, 60));
        // The latest the tick fires: the bound, a start up to a second
        // before its whole-second stamp, a tick up to a second late.
        assert!(ANSWER_SECS + 2 < router_floor_secs);

        let offers = |acts: &[Act]| {
            acts.iter()
                .filter(|act| **act == send(Leg::Propagated))
                .count()
        };
        let answer = ANSWER_SECS as i64;
        // (a) The tick first: called off, the router's end of the leg it
        // called off finds nothing, the next tick offers the copy.
        let mut outgoing = outbox();
        let mut acts = outgoing.tick(1000);
        assert!(outgoing.unanswered(&ID));
        acts.extend(outgoing.tick(1000 + answer));
        assert_eq!(acts.last(), Some(&unanswered(true)));
        acts.extend(outgoing.left(&ID, Outcome::Failed, 1000 + answer));
        for now in 1000 + answer + 1..1000 + 2 * router_secs as i64 {
            acts.extend(outgoing.tick(now));
        }
        assert_eq!(offers(&acts), 1, "{acts:?}");
        assert_eq!(outgoing.in_flight(&ID), Some(Leg::Propagated));
        // (b) The router first, a second before the bound: the same fact,
        // the copy goes at once and the bound finds no direct leg to cut.
        let mut outgoing = outbox();
        let mut acts = outgoing.tick(1000);
        acts.extend(outgoing.tick(1000 + answer - 1));
        assert!(outgoing.unanswered(&ID));
        acts.extend(outgoing.left(&ID, Outcome::Failed, 1000 + answer - 1));
        assert!(!outgoing.unanswered(&ID));
        for now in 1000 + answer..1000 + 2 * router_secs as i64 {
            acts.extend(outgoing.tick(now));
        }
        assert!(
            !acts.iter().any(|act| matches!(act, Act::Unanswered { .. })),
            "{acts:?}"
        );
        assert_eq!(offers(&acts), 1, "{acts:?}");
        assert_eq!(outgoing.in_flight(&ID), Some(Leg::Propagated));
    }

    /// 1330, miauhaus 12:15-12:19: a copy handed on to the node, its
    /// recipient offline, went direct again every 100 s: each unanswered
    /// leg's path request drew a path response from the cache, and the
    /// response started the next direct leg. A path response starts
    /// nothing, whether the copy is on the node, owed to it or never
    /// offered; the announce and the hour still try direct.
    #[test]
    fn a_path_response_does_not_try_direct_again() {
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        assert_eq!(outgoing.tick(1045), [unanswered(true)]);
        assert_eq!(outgoing.tick(1046), [send(Leg::Propagated)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Sent, 1065),
            [record(Change::Propagated)]
        );
        // Handed on: the response to the router's path request is no
        // reason to try direct.
        assert!(outgoing
            .announced(PEER, Heard::PathResponse, 1110)
            .is_empty());
        assert!(outgoing.tick(1110).is_empty());
        // Control: the recipient's own announce is.
        assert_eq!(
            outgoing.announced(PEER, Heard::Announce, 1200),
            [send(Leg::Direct)]
        );
        assert_eq!(outgoing.tick(1245), [unanswered(false)]);
        assert!(outgoing
            .announced(PEER, Heard::PathResponse, 1300)
            .is_empty());
        // And the cadence, counted from the announce's leg, doubled for
        // the one miss before it (1585).
        assert!(outgoing.tick(1200 + 7199).is_empty());
        assert_eq!(outgoing.tick(1200 + 7200), [send(Leg::Direct)]);
    }

    /// 1406, the field mailbox: a message the node took long ago, its
    /// direct leg unanswered every hour, offers the node nothing, and the
    /// act says so, so the daemon's line does not claim an offer. Control:
    /// one never handed on offers the copy.
    #[test]
    fn an_unanswered_leg_of_a_message_on_the_node_offers_nothing() {
        let mut outgoing = Outgoing::new(Policy::default());
        outgoing.add(ID, PEER, Vec::new(), 1000, true);
        assert!(outgoing.propagated(&ID));
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        assert_eq!(outgoing.tick(1045), [unanswered(false)]);
        assert!(outgoing.tick(1046).is_empty(), "no copy offered");
        let mut control = outbox();
        assert!(!control.propagated(&ID));
        assert_eq!(control.tick(1000), [send(Leg::Direct)]);
        assert_eq!(control.tick(1045), [unanswered(true)]);
        assert_eq!(control.tick(1046), [send(Leg::Propagated)]);
    }

    /// 1406: a propagated leg the engine could not start (the recipient's
    /// key is not held, said at once) ends: nothing more until the
    /// recipient is heard or the hour comes round, and the hour's direct
    /// leg asks the path again. The recipient's path response, which brings
    /// the key, offers the node the copy, once; its announce tries direct.
    /// Control: after a propagated leg that failed otherwise, a path
    /// response starts nothing (1330).
    #[test]
    fn a_propagated_leg_without_a_key_waits_for_the_key_or_the_hour() {
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        assert_eq!(outgoing.tick(1045), [unanswered(true)]);
        assert_eq!(outgoing.tick(1046), [send(Leg::Propagated)]);
        assert!(outgoing.left(&ID, Outcome::NoKey, 1046).is_empty());
        assert_eq!(outgoing.in_flight(&ID), None);
        for now in 1047..1000 + 3600 {
            assert!(outgoing.tick(now).is_empty(), "at {now}");
        }
        assert_eq!(outgoing.tick(1000 + 3600), [send(Leg::Direct)]);
        assert_eq!(outgoing.tick(1000 + 3645), [unanswered(true)]);
        assert_eq!(outgoing.tick(1000 + 3646), [send(Leg::Propagated)]);
        assert!(outgoing.left(&ID, Outcome::NoKey, 1000 + 3646).is_empty());
        // The path response brings the key: the node is offered the copy,
        // once.
        assert_eq!(
            outgoing.announced(PEER, Heard::PathResponse, 1000 + 3650),
            [send(Leg::Propagated)]
        );
        assert!(outgoing.left(&ID, Outcome::Failed, 1000 + 3651).is_empty());
        assert!(outgoing
            .announced(PEER, Heard::PathResponse, 1000 + 3660)
            .is_empty());
        // The recipient's own announce tries direct.
        assert_eq!(
            outgoing.announced(PEER, Heard::Announce, 1000 + 3700),
            [send(Leg::Direct)]
        );
        // A path response for another recipient offers nothing.
        let mut other = outbox();
        other.tick(1000);
        other.tick(1045);
        other.tick(1046);
        other.left(&ID, Outcome::NoKey, 1046);
        assert!(other
            .announced([7; 16], Heard::PathResponse, 1050)
            .is_empty());
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
