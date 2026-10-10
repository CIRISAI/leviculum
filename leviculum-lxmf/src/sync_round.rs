//! The outbound sync round's next step: what a propagation node does with
//! the round it opened to a peer when the link comes up, a request times
//! out, the round's deadline passes or the dial is refused.
//!
//! The board's engine (`leviculum-nrf/src/pn.rs`: `on_sync_link_up`, the
//! `RequestTimedOut` arm of `on_event`, `tick_sync` and `start_round`) took
//! these decisions inline until leviculum#413, and lnpnd
//! (`lnpnd/src/peering.rs`: `on_tick`, `start_round`,
//! `on_link_established`, `on_request_timeout`) kept its own copy of them
//! until #583. Both now take every step here, so a PC and a board peer the
//! same way. Every arm but the happy path
//! is a failure no rig cell induces: a peer that identifies and then
//! vanishes, a link whose identify cannot be sent, a request that times
//! out mid-round, a dial the link cap refuses. Here each is a host test
//! with the inputs the field produces.
//!
//! The step decides; the caller acts. An [`RoundStep::End`] is the
//! caller's `PN_SYNC ... result=<end>` line, the peer back to
//! [`SyncPhase::Idle`](crate::peering::SyncPhase::Idle) and the link
//! closed. What the peer's sync booking
//! ([`Peer::book_sync_attempt`](crate::peering::Peer::book_sync_attempt))
//! does is decided where the link comes up, not here: the caller notes the
//! link established ([`Peer::note_link_established`](crate::peering::Peer::note_link_established))
//! exactly when the identify went out, so a round that ends before that
//! leaves the peer deferred by its whole accumulator, and one that ends
//! after leaves it due on the next sync pass.

use leviculum_core::link::LinkId;

/// How long an outbound round may run, dial to conclusion, before it is
/// ended as [`RoundEnd::Timeout`].
pub const OUTBOUND_ROUND_DEADLINE_MS: u64 = 180_000;

/// The round in flight, as the step needs to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundView {
    /// The link the round dialled.
    pub link_id: LinkId,
    /// The `/offer` request's id, once sent.
    pub request_id: Option<[u8; 16]>,
    /// The monotonic millisecond past which the round is over
    /// ([`round_deadline_ms`]).
    pub deadline_ms: u64,
}

/// The deadline of a round dialled at `dialled_ms`.
pub const fn round_deadline_ms(dialled_ms: u64) -> u64 {
    dialled_ms.saturating_add(OUTBOUND_ROUND_DEADLINE_MS)
}

/// What just happened to the round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundEvent {
    /// The round's link came up and the caller tried to identify on it.
    LinkUp {
        /// Whether the identify went out.
        identified: bool,
        /// The peer's mined peering key, if it holds one.
        peering_key: Option<[u8; 32]>,
        /// The link's MDU, `None` when the core no longer holds the link.
        link_mdu: Option<usize>,
    },
    /// The core gave up on a request.
    RequestTimedOut {
        link_id: LinkId,
        request_id: [u8; 16],
    },
    /// A sync tick at this monotonic millisecond.
    Tick { now_ms: u64 },
    /// The dial was refused (the core's link cap).
    DialRefused,
    /// The store could not be walked for the entries above the peer's
    /// cursor: at round start, before anything was booked, or at link-up.
    StoreFailed,
}

/// Why a round ended without concluding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundEnd {
    /// The identify could not be sent on the fresh link.
    IdentifyFailed,
    /// The link came up but the peer holds no peering key to offer with.
    NoKey,
    /// The link was gone by the time the offer was to be sized against it.
    LinkGone,
    /// The `/offer` request timed out.
    OfferTimeout,
    /// The round outlived [`OUTBOUND_ROUND_DEADLINE_MS`].
    Timeout,
    /// The store walk for the offer failed.
    StoreError,
}

impl RoundEnd {
    /// The `result=` value of the round's `PN_SYNC` line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdentifyFailed => "identify_failed",
            Self::NoKey => "no_key",
            Self::LinkGone => "link_gone",
            Self::OfferTimeout => "offer_timeout",
            Self::Timeout => "timeout",
            Self::StoreError => "store_error",
        }
    }
}

/// What the round does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundStep {
    /// Nothing changes: no round, or the event is not this round's.
    Unchanged,
    /// Plan and send the `/offer` against this link.
    Offer {
        peering_key: [u8; 32],
        link_mdu: usize,
    },
    /// End the round: log the result, peer Idle, close the link.
    End(RoundEnd),
    /// No round was opened: peer Idle, nothing to close, and the booking
    /// made before the dial stands, so the peer is retried on a later
    /// pass instead of on every one.
    Defer,
    /// No round was opened and nothing was booked yet: book the peer's
    /// sync attempt
    /// ([`Peer::book_sync_attempt`](crate::peering::Peer::book_sync_attempt)),
    /// log the result, peer Idle. Without the booking the peer stays due
    /// and the next sync pass hits the same failure; with it the peer
    /// climbs the backoff ladder like any other unreachable one.
    Backoff(RoundEnd),
}

/// The round's next step on `event`; `round` is the round in flight, if
/// any.
///
/// At link-up the checks run in the order the link needs them: an identify
/// that did not go out ends the round before anything else, then a missing
/// peering key, then a link the core no longer holds. A request timeout
/// ends the round only when it is the round's own `/offer` on the round's
/// own link. The deadline is strict: a tick AT the deadline is still in
/// time. A store failure ends a round in flight and backs off a peer whose
/// round never opened.
pub fn round_step(round: Option<&RoundView>, event: RoundEvent) -> RoundStep {
    match event {
        RoundEvent::DialRefused => RoundStep::Defer,
        RoundEvent::StoreFailed => match round {
            Some(_) => RoundStep::End(RoundEnd::StoreError),
            None => RoundStep::Backoff(RoundEnd::StoreError),
        },
        RoundEvent::LinkUp {
            identified,
            peering_key,
            link_mdu,
        } => {
            if round.is_none() {
                return RoundStep::Unchanged;
            }
            if !identified {
                return RoundStep::End(RoundEnd::IdentifyFailed);
            }
            let Some(peering_key) = peering_key else {
                return RoundStep::End(RoundEnd::NoKey);
            };
            let Some(link_mdu) = link_mdu else {
                return RoundStep::End(RoundEnd::LinkGone);
            };
            RoundStep::Offer {
                peering_key,
                link_mdu,
            }
        }
        RoundEvent::RequestTimedOut {
            link_id,
            request_id,
        } => match round {
            Some(round) if round.link_id == link_id && round.request_id == Some(request_id) => {
                RoundStep::End(RoundEnd::OfferTimeout)
            }
            _ => RoundStep::Unchanged,
        },
        RoundEvent::Tick { now_ms } => match round {
            Some(round) if now_ms > round.deadline_ms => RoundStep::End(RoundEnd::Timeout),
            _ => RoundStep::Unchanged,
        },
    }
}

#[cfg(test)]
#[path = "sync_round_tests.rs"]
mod tests;
