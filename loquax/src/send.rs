//! The frontend of `loquax send`: everything the command does, expressed
//! against [`Outbox`] alone.
//!
//! Nothing in this module knows about Reticulum, LXMF, channels or a terminal.
//! That is what makes the whole command testable without a network, and it is
//! the same boundary the architecture record demands of the eventual TUI —
//! "the core must not know it has a terminal", decided 2026-08-08
//! (`docs/src/concepts/loquax-architecture.md` §4).
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
//! (`loquax-architecture.md` §2, trap 3), so no word this program prints is
//! ever "delivered to" a person.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use core::time::Duration;

use leviculum_lxmf::router::MessageState;

use crate::clock::{self, Clock};
use crate::effects::Effects;
use crate::events;
use crate::mailbox_state::{self, MailboxRun};
use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone, SendRequest, Via};

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
    /// How the mailbox is selected without `--pn`, and where it is kept.
    pub mailbox: MailboxRun,
}

impl SendOptions {
    /// The defaults `main` uses, with `budget` from `--timeout`.
    pub fn new(instance: impl Into<String>, budget: Duration) -> Self {
        Self {
            instance: instance.into(),
            budget,
            poll: Duration::from_millis(20),
            mailbox: MailboxRun::default(),
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

/// Exit codes, and the claims they are allowed to make. `loquax send`'s code
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
    /// The message went onto a link to the peer (`Sending`), and no delivery
    /// proof came back inside the budget, or the router gave up retrying it
    /// there. The peer answered the link, so it is reachable: this is "sent,
    /// not proven", and never a reason to hand a second copy to a mailbox
    /// (leviculum 1002).
    Unproven {
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

impl core::fmt::Display for SendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
                 Name one with --pn <hash>, or set propagation_node in Loquax's config."
            ),
            Self::Unproven { message_id, last } => write!(
                f,
                "{} went onto a link to the peer and no delivery proof came back ({}); \
                 the peer may have it.\n  \
                 It is not handed to a mailbox as well, which would deliver it twice.",
                crate::address::to_hex(message_id),
                match last {
                    Some(state) => format!("last state {state:?}"),
                    None => "no state reported".to_string(),
                }
            ),
        }
    }
}

impl core::error::Error for SendError {}

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

/// The error for a message the router gave up on. A plain `Failed` after the
/// message was on a link is the router exhausting its retries against a peer
/// that answered the link and never proved the packet: still "sent, not
/// proven", for the reason [`SendError::Unproven`] gives. A refusal stays a
/// refusal.
fn gave_up(message_id: [u8; 32], state: MessageState, on_link: bool) -> SendError {
    if on_link && state == MessageState::Failed {
        SendError::Unproven {
            message_id,
            last: Some(state),
        }
    } else {
        SendError::Rejected { message_id, state }
    }
}

/// Which leg of a send is running, for the log and for a frontend that shows
/// it ([`Effects::leg`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    /// Straight to the peer.
    Direct,
    /// Through a propagation node.
    Propagated,
}

impl Leg {
    /// The `leg=` word of the `SEND` log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Propagated => "propagated",
        }
    }
}

/// The `state=` word of the `SEND` log line for a router state.
fn leg_word(state: MessageState) -> &'static str {
    match state {
        MessageState::Generating | MessageState::Outbound => "queued",
        MessageState::Sending => "sending",
        MessageState::Sent | MessageState::AwaitingCollection => "sent",
        MessageState::Delivered => "proof",
        MessageState::Rejected | MessageState::Cancelled | MessageState::Failed => "failed",
    }
}

/// What the run is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Resolved,
    Queued,
    HandedOn,
}

/// Run one `loquax send`.
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
    clock: &impl Clock,
) -> Result<Queued, SendError> {
    run_send_steps(outbox, request, options, clock, &mut |_| {}).await
}

/// [`run_send`], each `SEND` line it logs also handed to `step`: the auto
/// send passes them to its [`Effects::step`].
async fn run_send_steps<O: Outbox>(
    outbox: &mut O,
    request: SendRequest,
    options: &SendOptions,
    clock: &impl Clock,
    step: &mut impl FnMut(&str),
) -> Result<Queued, SendError> {
    let start = clock.now_ms();
    let destination = request.destination;
    let via = request.via;
    let body_len = request.body.len();
    let mut leg = |word: &str| {
        step(&events::send_leg(
            Leg::Direct,
            &destination,
            word,
            clock::since(clock, start),
        ))
    };

    let mut phase = Phase::Ready;
    let mut address = None;
    let mut message_id = None;
    let mut last_state = None;
    // Whether the message reached `Sending`: handed to a link (or a
    // transfer on one) that the peer answered. From then on a missing
    // proof is not a missing peer.
    let mut on_link = false;
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
                        leg("path_wait");
                    }
                }
                OutboxEvent::Broken { detail } => return Err(SendError::Broken(detail)),
                OutboxEvent::Resolved { destination: which } if which == destination => {
                    if phase == Phase::Resolved {
                        events::resolved(
                            &destination,
                            clock::since(clock, start).as_millis() as u64,
                        );
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
                    leg(leg_word(state));
                    last_state = Some(state);
                    on_link |= state == MessageState::Sending;
                    if handed_on(state) {
                        return Ok(Queued {
                            message_id: id,
                            address: address.unwrap_or_default(),
                            state,
                        });
                    }
                    if given_up(state) {
                        return Err(gave_up(id, state, on_link));
                    }
                }
                OutboxEvent::State { .. } | OutboxEvent::Progress { .. } => {}
                OutboxEvent::Left {
                    message_id: id,
                    last,
                    ..
                } if Some(id) == message_id => {
                    // The router removes an entry only in a terminal state, so
                    // a removal we did not already act on means the terminal
                    // state's event and the removal arrived together.
                    let state = last.or(last_state).unwrap_or(MessageState::Failed);
                    return if given_up(state) {
                        Err(gave_up(id, state, on_link))
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
                | OutboxEvent::Mailbox(_)
                | OutboxEvent::Received { .. }
                | OutboxEvent::Propagated { .. }
                | OutboxEvent::Unverified { .. }
                | OutboxEvent::Verified { .. }
                | OutboxEvent::Announced { .. }
                | OutboxEvent::NodeAnnounced { .. }
                | OutboxEvent::Uploaded { .. }
                | OutboxEvent::MeshPosition { .. }
                | OutboxEvent::HubAnnounced { .. }
                | OutboxEvent::PageAnnounced { .. }
                | OutboxEvent::NameHeard { .. }
                | OutboxEvent::HubRooms { .. }
                | OutboxEvent::SyncLinked
                | OutboxEvent::SyncDone { .. }
                | OutboxEvent::SyncFailed { .. }
                | OutboxEvent::Unsent { .. }
                | OutboxEvent::KeyMissing { .. }
                | OutboxEvent::Fenced { .. }
                | OutboxEvent::Census(_)
                | OutboxEvent::Packed { .. }
                | OutboxEvent::HomeSent { .. }
                | OutboxEvent::HomeMarked { .. }
                | OutboxEvent::HomeStalled { .. }
                | OutboxEvent::HomePnStatus { .. }
                | OutboxEvent::HomePetname { .. }
                | OutboxEvent::HomePeerState { .. } => {}
            }
        }

        if clock::since(clock, start) >= options.budget {
            if phase == Phase::Resolved {
                events::path_not_found(&destination, clock::since(clock, start));
            }
            leg(if on_link { "no_proof" } else { "failed" });
            return Err(match (phase, message_id) {
                (Phase::Ready, _) => SendError::NeverReady,
                (Phase::Resolved, _) => SendError::Unreachable {
                    destination,
                    waited: clock::since(clock, start),
                },
                (Phase::Queued, _) => SendError::NoAnswer {
                    message_id: [0u8; 32],
                },
                (Phase::HandedOn, Some(id)) if on_link => SendError::Unproven {
                    message_id: id,
                    last: last_state,
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
        clock::sleep(clock, options.poll).await;
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
        // The link came up and the message went onto it: the peer is there,
        // and the reference never re-sends such a message through a
        // propagation node on its own. It waits for the proof while the
        // message is `SENDING` (`reference/LXMF/LXMF/LXMRouter.py:2796`,
        // "Waiting for proof") and retries on a new link when the receipt
        // times out (`reference/LXMF/LXMF/LXMessage.py:616-621`). A mailbox
        // copy would reach the peer a second time; against a client that
        // receives and never proves it turned a delivered message red and
        // uploaded it to a foreign node (Lew's test, 2026-10-25).
        SendError::Unproven { .. } => None,
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
// Eight: the run's inputs and its three host seams (outbox, effects, clock),
// each passed separately so a host can bring any one of its own.
#[allow(clippy::too_many_arguments)]
pub async fn run_send_propagated<O: Outbox, E: Effects>(
    outbox: &mut O,
    request: SendRequest,
    preferred_pn: Option<[u8; 16]>,
    pn_source: &str,
    cancel: Option<[u8; 32]>,
    options: &SendOptions,
    effects: &mut E,
    clock: &impl Clock,
) -> Result<Queued, SendError> {
    let start = clock.now_ms();
    let destination = request.destination;
    let via = request.via;
    let body_len = request.body.len();
    let leg = |effects: &mut E, word: &str| {
        effects.step(&events::send_leg(
            Leg::Propagated,
            &destination,
            word,
            clock::since(clock, start),
        ))
    };

    if let Some(message_id) = cancel {
        outbox.submit(Command::Cancel { message_id })?;
    }
    outbox.submit(Command::SelectPn {
        preferred: preferred_pn,
        mailbox: options.mailbox.input.clone(),
    })?;
    leg(effects, "path_wait");

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
                        if pn_source == "flag" {
                            mailbox_state::record_user_choice(node, effects);
                        }
                        if let Some(request) = request.take() {
                            outbox.submit(Command::Send(Box::new(request)))?;
                        }
                        phase = PropagatedPhase::Queued;
                    }
                }
                OutboxEvent::PnUnavailable { detail } => {
                    return Err(SendError::NoPropagationNode(detail));
                }
                OutboxEvent::Mailbox(decision) => mailbox_state::report(&decision, effects),
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
                    leg(effects, leg_word(state));
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
                    ..
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

        if clock::since(clock, start) >= options.budget {
            leg(effects, "failed");
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
        clock::sleep(clock, options.poll).await;
    }
}

/// How a `--via auto` send ended well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoSent {
    /// The direct delivery link carried it: exit 0.
    Direct(Queued),
    /// A propagation node holds it: [`EXIT_PROPAGATED`].
    Propagated(Queued),
}

/// How a `--via auto` send failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoError {
    /// The direct leg failed in a way a mailbox cannot help with
    /// ([`fallback_reason`] said `None`); no fallback ran.
    Direct(SendError),
    /// Both legs failed.
    Both {
        direct: SendError,
        fallback: SendError,
    },
}

/// Run one `--via auto` send: the direct delivery link first, a propagation
/// node when none comes up. The same orchestration for every frontend, the
/// CLI's `send` and the TUI's compose line.
///
/// `request.via` is ignored: leg one is [`Via::Link`], forced, because the
/// link either comes up or it does not, and an opportunistic packet would
/// count as handed on with the peer unreachable, so the fallback would never
/// fire. `mailbox` is asked for the mailbox input only when the fallback
/// runs: it reads the daemon's path table, a round trip a direct success
/// should not pay for. A stranded direct copy is cancelled before the
/// upload, so the peer cannot receive the message twice if it reappears
/// while the upload runs.
// The propagated runner's eight, plus the lazy mailbox input.
#[allow(clippy::too_many_arguments)]
pub async fn run_send_auto<O, F, Fut>(
    outbox: &mut O,
    request: SendRequest,
    preferred_pn: Option<[u8; 16]>,
    pn_source: &str,
    options: &mut SendOptions,
    mailbox: F,
    effects: &mut impl Effects,
    clock: &impl Clock,
) -> Result<AutoSent, AutoError>
where
    O: Outbox,
    F: FnOnce() -> Fut,
    Fut: core::future::Future<Output = MailboxRun>,
{
    let destination = request.destination;
    let leg = |via: Via| SendRequest {
        via,
        ..request.clone()
    };
    effects.leg(Leg::Direct);
    let mut step = |line: &str| effects.step(line);
    let direct = run_send_steps(outbox, leg(Via::Link), options, clock, &mut step).await;
    let direct_error = match direct {
        Ok(queued) => {
            events::via("direct", "link-delivery");
            return Ok(AutoSent::Direct(queued));
        }
        Err(error) => error,
    };
    let Some(reason) = fallback_reason(&direct_error) else {
        return Err(AutoError::Direct(direct_error));
    };
    events::via("propagated", reason);
    effects.notice(&format!(
        "no direct delivery to {} ({reason}); trying a propagation node",
        crate::address::to_hex(&destination)
    ));
    let cancel = match &direct_error {
        SendError::Stranded { message_id, .. } => Some(*message_id),
        _ => None,
    };
    options.mailbox = mailbox().await;
    effects.leg(Leg::Propagated);
    match run_send_propagated(
        outbox,
        leg(Via::Propagated),
        preferred_pn,
        pn_source,
        cancel,
        options,
        effects,
        clock,
    )
    .await
    {
        Ok(queued) => Ok(AutoSent::Propagated(queued)),
        Err(fallback) => Err(AutoError::Both {
            direct: direct_error,
            fallback,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::clock::fake::Stepping;
    use crate::clock::Tokio;
    use crate::effects::fake::Recorded;
    use crate::outbox::fake::FakeOutbox;

    const DST: [u8; 16] = [0xab; 16];
    const ID: [u8; 32] = [0x5a; 32];

    fn request() -> SendRequest {
        SendRequest {
            destination: DST,
            title: b"status".to_vec(),
            body: b"disk 91%".to_vec(),
            fields: Vec::new(),
            via: Via::Direct,
        }
    }

    fn options() -> SendOptions {
        SendOptions {
            instance: "test".to_string(),
            budget: Duration::from_secs(5),
            poll: Duration::from_millis(1),
            mailbox: MailboxRun::default(),
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

        let queued = run_send(&mut outbox, request(), &options(), &Tokio::new())
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
        let queued = run_send(&mut outbox, request(), &options(), &Tokio::new())
            .await
            .expect("delivered is handed on");
        assert_eq!(queued.state, MessageState::Delivered);
    }

    /// The negative case the brief names: a destination that does not exist
    /// must not produce a success, and must not queue anything.
    #[tokio::test(start_paused = true)]
    async fn an_unreachable_destination_fails_without_queueing_anything() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Ready { address: [1; 16] }]);

        let error = run_send(&mut outbox, request(), &options(), &Tokio::new())
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
        let error = run_send(&mut outbox, request(), &options(), &Tokio::new())
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
        let error = run_send(&mut outbox, request(), &options(), &Tokio::new())
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
        let error = run_send(&mut outbox, request(), &options(), &Tokio::new())
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

    /// The budget is the clock's, not tokio's: on a clock that moves only
    /// when the runner sleeps, a 5 s budget polled every 1 ms ends after
    /// exactly 5 000 sleeps, and the destination that never resolved says
    /// how long it was waited for.
    #[tokio::test]
    async fn the_budget_is_measured_on_the_clock_it_is_given() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Ready { address: [1; 16] }]);
        let clock = Stepping::default();
        let error = run_send(&mut outbox, request(), &options(), &clock)
            .await
            .expect_err("an unresolved destination is not a success");
        assert_eq!(
            error,
            SendError::Unreachable {
                destination: DST,
                waited: Duration::from_secs(5),
            }
        );
        assert_eq!(clock.sleeps.get(), 5_000);
    }

    #[tokio::test(start_paused = true)]
    async fn a_broken_engine_is_reported_verbatim() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Broken {
            detail: "register delivery destination".to_string(),
        }]);
        let error = run_send(&mut outbox, request(), &options(), &Tokio::new())
            .await
            .expect_err("a broken engine is a failure");
        assert!(matches!(error, SendError::Broken(_)), "{error:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_never_comes_up_fails_before_queueing_anything() {
        let mut outbox = FakeOutbox::new(Vec::new());
        let error = run_send(&mut outbox, request(), &options(), &Tokio::new())
            .await
            .expect_err("no readiness is a failure");
        assert_eq!(error, SendError::NeverReady);
        assert!(outbox.commands.borrow().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_dead_engine_is_reported_rather_than_waited_out() {
        let mut outbox = FakeOutbox::new(Vec::new());
        outbox.gone = true;
        let error = run_send(&mut outbox, request(), &options(), &Tokio::new())
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
            SendError::Unproven {
                message_id: ID,
                last: Some(MessageState::Sending),
            },
        ] {
            assert_eq!(fallback_reason(&stay), None, "{stay:?} must not fall back");
        }
    }

    /// What Lew's test against a client that receives and never proves came
    /// to (leviculum 1002): the link came up, the message went onto it, and
    /// no proof followed. That is "sent, not proven", and `--via auto` must
    /// neither call it a failure nor upload a second copy to a mailbox. The
    /// fallback would have run here before: the budget ended on `Stranded`,
    /// whose reason word is `stranded`.
    #[tokio::test(start_paused = true)]
    async fn a_message_on_the_link_without_a_proof_is_unproven_and_never_falls_back() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::Resolved { destination: DST },
            OutboxEvent::Queued { message_id: ID },
            OutboxEvent::State {
                message_id: ID,
                state: MessageState::Sending,
            },
        ]);
        let mut effects = Recorded::default();
        let mut asked_for_a_mailbox = false;

        let outcome = run_send_auto(
            &mut outbox,
            request(),
            Some(PN),
            "flag",
            &mut options(),
            || {
                asked_for_a_mailbox = true;
                async { MailboxRun::default() }
            },
            &mut effects,
            &Tokio::new(),
        )
        .await;

        assert!(
            matches!(
                outcome,
                Err(AutoError::Direct(SendError::Unproven {
                    message_id: ID,
                    last: Some(MessageState::Sending),
                }))
            ),
            "{outcome:?}"
        );
        assert!(!asked_for_a_mailbox, "the mailbox leg must not start");
        let commands = outbox.commands.borrow();
        assert!(
            !commands
                .iter()
                .any(|c| matches!(c, Command::SelectPn { .. } | Command::Cancel { .. })),
            "no node selected, the direct copy not withdrawn: {commands:?}"
        );
        assert!(effects.notices.is_empty(), "{:?}", effects.notices);
    }

    /// The router retrying on new links after each receipt timeout, and then
    /// giving up, inside the budget: the message was still on a link the peer
    /// answered, so this is unproven too, not a reason to fall back. A
    /// refusal by the peer stays a refusal.
    #[tokio::test(start_paused = true)]
    async fn giving_up_after_the_link_carried_it_is_unproven_and_a_refusal_is_not() {
        let after_link = |end: MessageState| {
            FakeOutbox::new(vec![
                OutboxEvent::Ready { address: [1; 16] },
                OutboxEvent::Resolved { destination: DST },
                OutboxEvent::Queued { message_id: ID },
                OutboxEvent::State {
                    message_id: ID,
                    state: MessageState::Sending,
                },
                OutboxEvent::State {
                    message_id: ID,
                    state: MessageState::Outbound,
                },
                OutboxEvent::State {
                    message_id: ID,
                    state: end,
                },
            ])
        };
        let failed = run_send(
            &mut after_link(MessageState::Failed),
            request(),
            &options(),
            &Tokio::new(),
        )
        .await;
        assert_eq!(
            failed,
            Err(SendError::Unproven {
                message_id: ID,
                last: Some(MessageState::Failed),
            })
        );
        let rejected = run_send(
            &mut after_link(MessageState::Rejected),
            request(),
            &options(),
            &Tokio::new(),
        )
        .await;
        assert_eq!(
            rejected,
            Err(SendError::Rejected {
                message_id: ID,
                state: MessageState::Rejected,
            })
        );
    }

    const PN: [u8; 16] = [0x77; 16];

    fn propagated_request() -> SendRequest {
        SendRequest {
            destination: DST,
            title: b"status".to_vec(),
            body: b"disk 91%".to_vec(),
            fields: Vec::new(),
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
            &mut Recorded::default(),
            &Tokio::new(),
        )
        .await
        .expect("an accepted upload is the success");

        assert_eq!(queued.state, MessageState::AwaitingCollection);
        let commands = outbox.commands.borrow();
        assert!(
            matches!(commands[0], Command::SelectPn { preferred: Some(p), .. } if p == PN),
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
            "adopted",
            None,
            &options(),
            &mut Recorded::default(),
            &Tokio::new(),
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
                failure: None,
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
            &mut Recorded::default(),
            &Tokio::new(),
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
            &mut Recorded::default(),
            &Tokio::new(),
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

    /// A direct leg the peer proves leaves `path_wait`, `sending` and
    /// `proof`, each with its `elapsed_ms`, in that order: the steps a
    /// delivered message's log must show (leviculum 1014). Read off the
    /// effects, which get the very lines the log writes: a capturing
    /// tracing subscriber in a parallel test run loses lines to the
    /// callsite interest another thread cached without it.
    #[tokio::test(start_paused = true)]
    async fn a_proven_direct_leg_logs_path_wait_sending_and_proof() {
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
        let mut effects = Recorded::default();
        let sent = run_send_auto(
            &mut outbox,
            request(),
            None,
            "none",
            &mut options(),
            || async { MailboxRun::default() },
            &mut effects,
            &Stepping::default(),
        )
        .await
        .expect("a proven direct leg is a success");
        assert!(matches!(sent, AutoSent::Direct(_)), "{sent:?}");
        assert!(effects.notices.is_empty(), "{:?}", effects.notices);
        assert_eq!(
            effects.steps,
            [
                "SEND leg=direct to=abababab state=path_wait elapsed_ms=0",
                "SEND leg=direct to=abababab state=sending elapsed_ms=0",
                "SEND leg=direct to=abababab state=proof elapsed_ms=0",
            ]
        );
    }
}
