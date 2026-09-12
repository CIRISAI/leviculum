//! The frontend of `lnmsg send`: everything the command does, expressed
//! against [`Outbox`] alone.
//!
//! Nothing in this module knows about Reticulum, LXMF, channels or a terminal.
//! That is what makes the whole command testable without a network, and it is
//! the same boundary the architecture record demands of the eventual TUI —
//! "the core must not know it has a terminal", decided 2026-08-08
//! (`docs/src/concepts/lnmsg-architecture.md` §4).
//!
//! # What exit 0 is allowed to mean
//!
//! Decision 7 says exit 0 means "queued cleanly" and claims nothing more; that
//! half is untouched and is what this module exists to keep true. Its other
//! half — return immediately, with the message id on stdout — is gone: the id
//! came off stdout on 2026-08-21 (a success now prints nothing), and the
//! immediate return did not survive contact with the process model decided two
//! days before the decision. In option A there is no daemon, so the process
//! that holds the outbound queue is the process that is about to exit. A
//! literal immediate return would report success and then destroy the queue
//! that success referred to, which is a worse lie than the one the decision
//! was written to prevent.
//!
//! So this waits — bounded, and for the earliest thing that makes the claim
//! true rather than for a delivery proof. The message is *handed on* when the
//! router reports [`MessageState::Sent`] (the bytes went to Reticulum),
//! [`MessageState::Delivered`] (a transport proof came back) or
//! [`MessageState::AwaitingCollection`] (a propagation node took it). Any of
//! those ends the run at once, so a working send does not block until a proof
//! arrives, which is what decision 7 was protecting. The deviation is recorded
//! in the batch report.
//!
//! `Delivered` is a Reticulum transport proof and not an application receipt
//! (`lnmsg-architecture.md` §2, trap 3), so no word this program prints is
//! ever "delivered to" a person.

use std::time::Duration;

// `tokio::time::Instant`, not `std::time::Instant`: the budget has to be
// measured on the same clock the sleep below uses, so a test can drive a
// thirty-second budget through in milliseconds with a paused runtime clock and
// still exercise the real expiry path.
use tokio::time::Instant;

use leviculum_lxmf::router::MessageState;

use crate::events;
use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone, SendRequest};

/// How long a run may take once the daemon connection is up, and how often the
/// event queue is looked at.
#[derive(Debug, Clone)]
pub struct SendOptions {
    /// The shared-instance name, for the log line only.
    pub instance: String,
    /// Total budget for everything after attaching: becoming ready, resolving
    /// the destination, queueing, and getting the message onto the network.
    /// One budget rather than one per phase, because what a cron job cares
    /// about is when the command is guaranteed to be gone.
    pub budget: Duration,
    /// How long to sleep when the event queue is empty.
    ///
    /// The engine's own cadence is 200 ms and its events arrive on an
    /// unbounded queue, so nothing is lost by looking less often than it
    /// speaks; 20 ms keeps the added latency well under the cost of one
    /// packet on any medium this runs over.
    pub poll: Duration,
}

impl SendOptions {
    /// The defaults `main` uses, with `budget` from `--timeout`.
    pub fn new(instance: impl Into<String>, budget: Duration) -> Self {
        Self {
            instance: instance.into(),
            budget,
            poll: Duration::from_millis(20),
        }
    }
}

/// A message that was queued and handed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub message_id: [u8; 32],
    /// Our own address — what a reply to this message would be sent to.
    pub address: [u8; 16],
    /// The state that ended the run.
    pub state: MessageState,
}

/// Exit codes, and the claims they are allowed to make. `lnmsg send`'s code
/// distinguishes the three outcomes a script has to tell apart: the message
/// was handed on towards the recipient directly (0), it is waiting in a
/// propagation node's mailbox (3), or neither happened (1). 2 stays the
/// argument-error code, per `lnomad`'s convention (`lnomad/src/main.rs:174`).
pub const EXIT_DIRECT: u8 = 0;
/// Neither direct delivery nor a mailbox took the message.
pub const EXIT_FAILURE: u8 = 1;
/// The command line was wrong; nothing was attempted.
pub const EXIT_USAGE: u8 = 2;
/// A propagation node accepted the message for later collection. Not 0:
/// "the recipient's path has the bytes" and "a mailbox is holding them until
/// the recipient asks" are different promises, and a cron job pointing at a
/// sometimes-offline peer is exactly the caller that needs to know which one
/// it got.
pub const EXIT_PROPAGATED: u8 = 3;

/// Why a send did not get as far as being handed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The engine stopped answering.
    Gone,
    /// The engine reported it could not start.
    Broken(String),
    /// The engine never became ready inside the budget.
    NeverReady,
    /// The destination did not become reachable inside the budget.
    Unreachable {
        destination: [u8; 16],
        waited: Duration,
    },
    /// The router refused the message. Nothing was queued.
    Refused(String),
    /// The router accepted the message and then said nothing at all.
    NoAnswer { message_id: [u8; 32] },
    /// The router gave up on the message.
    Rejected {
        message_id: [u8; 32],
        state: MessageState,
    },
    /// The budget ran out with the message still in this process's queue.
    /// Since this process *is* the queue, that means it did not go out.
    Stranded {
        message_id: [u8; 32],
        last: Option<MessageState>,
    },
    /// No propagation node could be selected, so a propagated send had
    /// nowhere to go. The detail names which of the three sources (flag,
    /// config, announces) came up empty, or what the selection said.
    NoPropagationNode(String),
}

impl From<OutboxGone> for SendError {
    fn from(_: OutboxGone) -> Self {
        Self::Gone
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => write!(f, "the message engine stopped answering"),
            Self::Broken(detail) => write!(f, "the message engine could not start: {detail}"),
            Self::NeverReady => write!(
                f,
                "the message engine did not come up inside the timeout; nothing was queued"
            ),
            Self::Unreachable {
                destination,
                waited,
            } => write!(
                f,
                "no route to {} after {:.1}s: no announce from it has reached this node, \
                 so there is no key to encrypt to and no path to send over.\n  \
                 Nothing was queued.",
                crate::address::to_hex(destination),
                waited.as_secs_f32()
            ),
            Self::Refused(detail) => write!(f, "the message was refused: {detail}"),
            Self::NoAnswer { message_id } => write!(
                f,
                "{} was queued and the engine then reported nothing before the timeout",
                crate::address::to_hex(message_id)
            ),
            Self::Rejected { message_id, state } => write!(
                f,
                "{} was queued and the router gave up on it ({state:?}); it did not go out",
                crate::address::to_hex(message_id)
            ),
            Self::Stranded { message_id, last } => write!(
                f,
                "{} was still waiting in this process's queue at the timeout ({}), \
                 and this process holds the queue: it did not go out.\n  \
                 Raise --timeout, or check that the destination is still reachable.",
                crate::address::to_hex(message_id),
                match last {
                    Some(state) => format!("last state {state:?}"),
                    None => "no state reported".to_string(),
                }
            ),
            Self::NoPropagationNode(detail) => write!(
                f,
                "no propagation node: {detail}.\n  \
                 Name one with --pn <hash>, or set propagation_node in lnmsg's config."
            ),
        }
    }
}

impl std::error::Error for SendError {}

/// A state that means the bytes left this process.
fn handed_on(state: MessageState) -> bool {
    matches!(
        state,
        MessageState::Sent | MessageState::Delivered | MessageState::AwaitingCollection
    )
}

/// A state the router does not come back from.
fn given_up(state: MessageState) -> bool {
    matches!(
        state,
        MessageState::Failed | MessageState::Rejected | MessageState::Cancelled
    )
}

/// What the run is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Resolved,
    Queued,
    HandedOn,
}

/// Run one `lnmsg send`.
///
/// It writes to no stream at all, which is how the "a success prints nothing"
/// rule is held: there is no sink here to print an id to, so no future edit
/// can quietly reintroduce one. The id reaches whoever wants it through
/// `LNMSG_ENQUEUED id=…` in the structured event log, and through the returned
/// [`Queued`].
pub async fn run_send<O: Outbox>(
    outbox: &mut O,
    request: SendRequest,
    options: &SendOptions,
) -> Result<Queued, SendError> {
    let start = Instant::now();
    let destination = request.destination;
    let via = request.via;
    let body_len = request.body.len();

    let mut phase = Phase::Ready;
    let mut address = None;
    let mut message_id = None;
    let mut last_state = None;
    let mut request = Some(request);

    loop {
        while let Some(event) = outbox.try_next_event()? {
            match event {
                OutboxEvent::Ready { address: ours } => {
                    address = Some(ours);
                    events::attached(&options.instance, &ours);
                    if phase == Phase::Ready {
                        outbox.submit(Command::Resolve { destination })?;
                        phase = Phase::Resolved;
                    }
                }
                OutboxEvent::Broken { detail } => return Err(SendError::Broken(detail)),
                OutboxEvent::Resolved { destination: which } if which == destination => {
                    if phase == Phase::Resolved {
                        events::resolved(&destination, start.elapsed().as_millis() as u64);
                        // `request` is taken exactly once: the resolve answer
                        // can only arrive in this phase, and the phase moves on
                        // in the same breath.
                        if let Some(request) = request.take() {
                            outbox.submit(Command::Send(Box::new(request)))?;
                        }
                        phase = Phase::Queued;
                    }
                }
                OutboxEvent::Resolved { .. } => {}
                OutboxEvent::Queued { message_id: id } => {
                    message_id = Some(id);
                    events::enqueued(&id, &destination, body_len, via.as_str());
                    phase = Phase::HandedOn;
                }
                OutboxEvent::Refused { detail } => return Err(SendError::Refused(detail)),
                OutboxEvent::State {
                    message_id: id,
                    state,
                } if Some(id) == message_id => {
                    events::state(&id, &format!("{state:?}"));
                    last_state = Some(state);
                    if handed_on(state) {
                        return Ok(Queued {
                            message_id: id,
                            address: address.unwrap_or_default(),
                            state,
                        });
                    }
                    if given_up(state) {
                        return Err(SendError::Rejected {
                            message_id: id,
                            state,
                        });
                    }
                }
                OutboxEvent::State { .. } => {}
                OutboxEvent::Left {
                    message_id: id,
                    last,
                } if Some(id) == message_id => {
                    // The router removes an entry only in a terminal state, so
                    // a removal we did not already act on means the terminal
                    // state's event and the removal arrived together.
                    let state = last.or(last_state).unwrap_or(MessageState::Failed);
                    return if given_up(state) {
                        Err(SendError::Rejected {
                            message_id: id,
                            state,
                        })
                    } else {
                        Ok(Queued {
                            message_id: id,
                            address: address.unwrap_or_default(),
                            state,
                        })
                    };
                }
                OutboxEvent::Left { .. } => {}
                // Mailbox traffic. A direct run has no fetch in flight, and a
                // node selection is the propagated runner's business.
                OutboxEvent::PnSelected { .. }
                | OutboxEvent::PnUnavailable { .. }
                | OutboxEvent::Received { .. }
                | OutboxEvent::SyncDone { .. }
                | OutboxEvent::SyncFailed { .. } => {}
            }
        }

        if start.elapsed() >= options.budget {
            return Err(match (phase, message_id) {
                (Phase::Ready, _) => SendError::NeverReady,
                (Phase::Resolved, _) => SendError::Unreachable {
                    destination,
                    waited: start.elapsed(),
                },
                (Phase::Queued, _) => SendError::NoAnswer {
                    message_id: [0u8; 32],
                },
                (Phase::HandedOn, Some(id)) => SendError::Stranded {
                    message_id: id,
                    last: last_state,
                },
                // Unreachable: the phase only becomes HandedOn together with
                // the id being set.
                (Phase::HandedOn, None) => SendError::NeverReady,
            });
        }
        tokio::time::sleep(options.poll).await;
    }
}

/// Whether — and why — a failed direct leg of `--via auto` should fall back
/// to a propagation node.
///
/// The word returned is the `reason=` value of the `LNMSG_VIA` line, so it
/// stays short and stable. `None` means the failure is not about the
/// destination at all (the engine never came up, the command line was
/// refused): uploading to a mailbox would fail the same way, or worse, hide
/// an operator error behind a queued copy nobody asked for.
pub fn fallback_reason(error: &SendError) -> Option<&'static str> {
    match error {
        // No route and no key: the peer is not reachable from here right
        // now, which is exactly the case a mailbox exists for.
        SendError::Unreachable { .. } => Some("no-route"),
        // A route existed but the message never left: the link did not come
        // up inside the budget.
        SendError::Stranded { .. } => Some("stranded"),
        // The router gave up on the direct attempt.
        SendError::Rejected { .. } => Some("rejected"),
        // Engine-level failures. `NoAnswer` is deliberately here too: an
        // engine that queued a message and then said nothing is wedged, and
        // handing a second message to the same wedged engine cannot help.
        SendError::Gone
        | SendError::Broken(_)
        | SendError::NeverReady
        | SendError::Refused(_)
        | SendError::NoAnswer { .. }
        | SendError::NoPropagationNode(_) => None,
    }
}

/// What the propagated runner is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropagatedPhase {
    Selecting,
    Queued,
    HandedOn,
}

/// Run one propagated send: select the node, queue the message, and wait for
/// the node to accept the upload (`MessageState::AwaitingCollection`).
///
/// `preferred_pn` is `--pn` or the configured default; `None` lets the
/// engine take the most recently announced node. `pn_source` is the word the
/// `LNMSG_PN` log line carries for where the choice came from. `cancel`
/// removes a stranded direct copy first, so an auto fallback cannot hand the
/// same message to the peer twice if the peer reappears mid-upload.
///
/// Unlike [`run_send`] this does not wait for the engine's `Ready`: it is
/// also called as the second leg of `--via auto`, where `Ready` was consumed
/// by the direct leg. Commands queue until the engine is ready either way,
/// so the sequencing is unchanged; only the events differ.
pub async fn run_send_propagated<O: Outbox>(
    outbox: &mut O,
    request: SendRequest,
    preferred_pn: Option<[u8; 16]>,
    pn_source: &str,
    cancel: Option<[u8; 32]>,
    options: &SendOptions,
) -> Result<Queued, SendError> {
    let start = Instant::now();
    let destination = request.destination;
    let via = request.via;
    let body_len = request.body.len();

    if let Some(message_id) = cancel {
        outbox.submit(Command::Cancel { message_id })?;
    }
    outbox.submit(Command::SelectPn {
        preferred: preferred_pn,
    })?;

    let mut phase = PropagatedPhase::Selecting;
    let mut address = None;
    let mut message_id = None;
    let mut last_state = None;
    let mut request = Some(request);

    loop {
        while let Some(event) = outbox.try_next_event()? {
            match event {
                OutboxEvent::Ready { address: ours } => {
                    address = Some(ours);
                    events::attached(&options.instance, &ours);
                }
                OutboxEvent::Broken { detail } => return Err(SendError::Broken(detail)),
                OutboxEvent::PnSelected {
                    destination: node,
                    stamp_cost,
                } => {
                    if phase == PropagatedPhase::Selecting {
                        events::pn(&node, pn_source, stamp_cost);
                        if let Some(request) = request.take() {
                            outbox.submit(Command::Send(Box::new(request)))?;
                        }
                        phase = PropagatedPhase::Queued;
                    }
                }
                OutboxEvent::PnUnavailable { detail } => {
                    return Err(SendError::NoPropagationNode(detail));
                }
                OutboxEvent::Queued { message_id: id } => {
                    if phase == PropagatedPhase::Queued && message_id.is_none() {
                        message_id = Some(id);
                        events::enqueued(&id, &destination, body_len, via.as_str());
                        phase = PropagatedPhase::HandedOn;
                    }
                }
                OutboxEvent::Refused { detail } => return Err(SendError::Refused(detail)),
                OutboxEvent::State {
                    message_id: id,
                    state,
                } if Some(id) == message_id => {
                    events::state(&id, &format!("{state:?}"));
                    last_state = Some(state);
                    if handed_on(state) {
                        return Ok(Queued {
                            message_id: id,
                            address: address.unwrap_or_default(),
                            state,
                        });
                    }
                    if given_up(state) {
                        return Err(SendError::Rejected {
                            message_id: id,
                            state,
                        });
                    }
                }
                OutboxEvent::Left {
                    message_id: id,
                    last,
                } if Some(id) == message_id => {
                    let state = last.or(last_state).unwrap_or(MessageState::Failed);
                    return if given_up(state) {
                        Err(SendError::Rejected {
                            message_id: id,
                            state,
                        })
                    } else {
                        Ok(Queued {
                            message_id: id,
                            address: address.unwrap_or_default(),
                            state,
                        })
                    };
                }
                // Stale events about the direct leg's message (its id never
                // matches ours), resolves, and mailbox traffic no send asked
                // for.
                _ => {}
            }
        }

        if start.elapsed() >= options.budget {
            return Err(match (phase, message_id) {
                (PropagatedPhase::Selecting, _) => SendError::NoPropagationNode(
                    "selecting a node did not finish inside the timeout".to_string(),
                ),
                (PropagatedPhase::Queued, _) => SendError::NoAnswer {
                    message_id: [0u8; 32],
                },
                (PropagatedPhase::HandedOn, Some(id)) => SendError::Stranded {
                    message_id: id,
                    last: last_state,
                },
                // Unreachable: the phase only becomes HandedOn together with
                // the id being set.
                (PropagatedPhase::HandedOn, None) => SendError::NeverReady,
            });
        }
        tokio::time::sleep(options.poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::outbox::fake::FakeOutbox;
    use crate::outbox::Via;

    const DST: [u8; 16] = [0xab; 16];
    const ID: [u8; 32] = [0x5a; 32];

    fn request() -> SendRequest {
        SendRequest {
            destination: DST,
            title: b"status".to_vec(),
            body: b"disk 91%".to_vec(),
            via: Via::Direct,
        }
    }

    fn options() -> SendOptions {
        SendOptions {
            instance: "test".to_string(),
            budget: Duration::from_secs(5),
            poll: Duration::from_millis(1),
        }
    }

    /// The happy path: the id comes back to the caller, and nowhere else. The
    /// "prints nothing" half of the contract is held by the signature — there
    /// is no stream here to print to — so what is left to assert is that the
    /// id still reaches the one caller that needs it.
    #[tokio::test(start_paused = true)]
    async fn a_handed_on_message_returns_its_id() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::Resolved { destination: DST },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Sent,
            },
        ]);

        let queued = run_send(&mut outbox, request(), &options())
            .await
            .expect("a handed-on message is a success");

        assert_eq!(queued.message_id, ID);
        assert_eq!(queued.state, MessageState::Sent);
        let commands = outbox.commands.borrow();
        assert_eq!(commands.len(), 2, "one resolve then one send: {commands:?}");
        assert!(matches!(commands[0], Command::Resolve { .. }));
        assert!(matches!(commands[1], Command::Send(_)));
    }

    /// A transport proof is also a success, and is still not called delivery
    /// anywhere the user can see.
    #[tokio::test(start_paused = true)]
    async fn a_delivery_proof_ends_the_run_too() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::Resolved { destination: DST },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Sending,
            },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Delivered,
            },
        ]);
        let queued = run_send(&mut outbox, request(), &options())
            .await
            .expect("delivered is handed on");
        assert_eq!(queued.state, MessageState::Delivered);
    }

    /// The negative case the brief names: a destination that does not exist
    /// must not produce a success, and must not queue anything.
    #[tokio::test(start_paused = true)]
    async fn an_unreachable_destination_fails_without_queueing_anything() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Ready { address: [1; 16] }]);

        let error = run_send(&mut outbox, request(), &options())
            .await
            .expect_err("an unresolvable destination is a failure");

        assert!(
            matches!(error, SendError::Unreachable { destination, .. } if destination == DST),
            "{error:?}"
        );
        assert_eq!(
            outbox.commands.borrow().len(),
            1,
            "the message must not be queued when the destination never resolved"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_message_is_an_error() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::Resolved { destination: DST },
            OutboxEvent::Refused {
                detail: "QueueFull".to_string(),
            },
        ]);
        let error = run_send(&mut outbox, request(), &options())
            .await
            .expect_err("a refusal is a failure");
        assert!(matches!(error, SendError::Refused(_)), "{error:?}");
    }

    /// The router giving up must not be reported as success just because the
    /// message got as far as being queued. The error names the id, which is
    /// what lets an operator find the message in the event log.
    #[tokio::test(start_paused = true)]
    async fn a_terminal_failure_after_queueing_is_still_a_failure() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::Resolved { destination: DST },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Failed,
            },
        ]);
        let error = run_send(&mut outbox, request(), &options())
            .await
            .expect_err("the router giving up is a failure");
        assert!(
            matches!(
                error,
                SendError::Rejected {
                    state: MessageState::Failed,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(
            error.to_string().contains(&crate::address::to_hex(&ID)),
            "the error must name the message that failed: {error}"
        );
    }

    /// The case the single-process model makes possible and a daemon would
    /// not: queued, never handed on, and the queue dies with the process.
    #[tokio::test(start_paused = true)]
    async fn a_message_still_queued_at_the_timeout_is_reported_as_stranded() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::Resolved { destination: DST },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Outbound,
            },
        ]);
        let error = run_send(&mut outbox, request(), &options())
            .await
            .expect_err("a stranded message is not a success");
        assert!(
            matches!(
                error,
                SendError::Stranded {
                    last: Some(MessageState::Outbound),
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_broken_engine_is_reported_verbatim() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Broken {
            detail: "register delivery destination".to_string(),
        }]);
        let error = run_send(&mut outbox, request(), &options())
            .await
            .expect_err("a broken engine is a failure");
        assert!(matches!(error, SendError::Broken(_)), "{error:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_never_comes_up_fails_before_queueing_anything() {
        let mut outbox = FakeOutbox::new(Vec::new());
        let error = run_send(&mut outbox, request(), &options())
            .await
            .expect_err("no readiness is a failure");
        assert_eq!(error, SendError::NeverReady);
        assert!(outbox.commands.borrow().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_dead_engine_is_reported_rather_than_waited_out() {
        let mut outbox = FakeOutbox::new(Vec::new());
        outbox.gone = true;
        let error = run_send(&mut outbox, request(), &options())
            .await
            .expect_err("a dead engine is a failure");
        assert_eq!(error, SendError::Gone);
    }

    /// Every error message a user can read has to name the message or the
    /// destination, and none of them may say the message was sent or
    /// delivered.
    #[test]
    fn no_error_wording_claims_delivery() {
        let errors = [
            SendError::Gone,
            SendError::Broken("x".into()),
            SendError::NeverReady,
            SendError::Unreachable {
                destination: DST,
                waited: Duration::from_secs(3),
            },
            SendError::Refused("QueueFull".into()),
            SendError::NoAnswer { message_id: ID },
            SendError::Rejected {
                message_id: ID,
                state: MessageState::Failed,
            },
            SendError::Stranded {
                message_id: ID,
                last: None,
            },
            SendError::NoPropagationNode("none announced".into()),
        ];
        for error in errors {
            let text = error.to_string().to_lowercase();
            assert!(
                !text.contains("delivered") && !text.contains("was sent"),
                "an error may not claim delivery: {text}"
            );
        }
    }

    /// The auto decision table. Destination-shaped failures fall back;
    /// engine-shaped ones do not, because a second message into the same
    /// broken engine cannot fare better.
    #[test]
    fn only_destination_shaped_failures_fall_back() {
        assert_eq!(
            fallback_reason(&SendError::Unreachable {
                destination: DST,
                waited: Duration::from_secs(3),
            }),
            Some("no-route")
        );
        assert_eq!(
            fallback_reason(&SendError::Stranded {
                message_id: ID,
                last: Some(MessageState::Outbound),
            }),
            Some("stranded")
        );
        assert_eq!(
            fallback_reason(&SendError::Rejected {
                message_id: ID,
                state: MessageState::Failed,
            }),
            Some("rejected")
        );
        for stay in [
            SendError::Gone,
            SendError::Broken("x".into()),
            SendError::NeverReady,
            SendError::Refused("QueueFull".into()),
            SendError::NoAnswer { message_id: ID },
            SendError::NoPropagationNode("none".into()),
        ] {
            assert_eq!(fallback_reason(&stay), None, "{stay:?} must not fall back");
        }
    }

    const PN: [u8; 16] = [0x77; 16];

    fn propagated_request() -> SendRequest {
        SendRequest {
            destination: DST,
            title: b"status".to_vec(),
            body: b"disk 91%".to_vec(),
            via: Via::Propagated,
        }
    }

    /// The propagated happy path: node selected, message queued, node
    /// accepted the upload. `AwaitingCollection` — not `Sent` — is what ends
    /// the run, because that is the state the upload-complete leg reports
    /// (`leviculum-lxmf/src/router/propagation_runtime.rs`, "Reporting
    /// `Sent` here would make this indistinguishable").
    #[tokio::test(start_paused = true)]
    async fn a_propagated_send_ends_on_awaiting_collection() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: Some(13),
            },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Sending,
            },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::AwaitingCollection,
            },
        ]);

        let queued = run_send_propagated(
            &mut outbox,
            propagated_request(),
            Some(PN),
            "flag",
            None,
            &options(),
        )
        .await
        .expect("an accepted upload is the success");

        assert_eq!(queued.state, MessageState::AwaitingCollection);
        let commands = outbox.commands.borrow();
        assert!(
            matches!(commands[0], Command::SelectPn { preferred: Some(p) } if p == PN),
            "selection first: {commands:?}"
        );
        assert!(matches!(commands[1], Command::Send(_)));
    }

    /// No node, no upload: the message must not be queued at all when the
    /// selection came up empty.
    #[tokio::test(start_paused = true)]
    async fn no_selectable_node_fails_before_queueing() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::PnUnavailable {
                detail: "none announced".to_string(),
            },
        ]);

        let error = run_send_propagated(
            &mut outbox,
            propagated_request(),
            None,
            "announced",
            None,
            &options(),
        )
        .await
        .expect_err("no node is a failure");

        assert!(
            matches!(error, SendError::NoPropagationNode(_)),
            "{error:?}"
        );
        assert!(
            !outbox
                .commands
                .borrow()
                .iter()
                .any(|command| matches!(command, Command::Send(_))),
            "nothing may be queued without a node"
        );
    }

    /// The auto fallback's cancel: a stranded direct copy is withdrawn
    /// before the propagated copy is queued, and stale events about it are
    /// not mistaken for the new message's.
    #[tokio::test(start_paused = true)]
    async fn the_fallback_cancels_the_stranded_direct_copy_first() {
        const DIRECT_ID: [u8; 32] = [0x0d; 32];
        let mut outbox = FakeOutbox::new(vec![
            // Stale traffic from the direct leg, arriving late.
            OutboxEvent::State {
                message_id: DIRECT_ID,
                state: MessageState::Cancelled,
            },
            OutboxEvent::Left {
                message_id: DIRECT_ID,
                last: Some(MessageState::Cancelled),
            },
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::AwaitingCollection,
            },
        ]);

        let queued = run_send_propagated(
            &mut outbox,
            propagated_request(),
            Some(PN),
            "flag",
            Some(DIRECT_ID),
            &options(),
        )
        .await
        .expect("stale direct events must not derail the fallback");

        assert_eq!(
            queued.message_id, ID,
            "the propagated copy's id, not the cancelled one's"
        );
        let commands = outbox.commands.borrow();
        assert!(
            matches!(commands[0], Command::Cancel { message_id } if message_id == DIRECT_ID),
            "the cancel goes first: {commands:?}"
        );
    }

    /// A node that accepts the link and then never finishes the upload is a
    /// stranded message, same as the direct path's version of the story.
    #[tokio::test(start_paused = true)]
    async fn an_upload_that_never_completes_is_stranded_at_the_timeout() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Sending,
            },
        ]);

        let error = run_send_propagated(
            &mut outbox,
            propagated_request(),
            Some(PN),
            "flag",
            None,
            &options(),
        )
        .await
        .expect_err("an unfinished upload is not a success");
        assert!(
            matches!(
                error,
                SendError::Stranded {
                    message_id: id,
                    last: Some(MessageState::Sending),
                } if id == ID
            ),
            "{error:?}"
        );
    }
}
