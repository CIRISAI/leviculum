//! The frontend of `loquax fetch`: drain the selected propagation node's
//! mailbox with the genuine list/fetch/confirm round, print what is new.
//!
//! Like [`crate::send::run_send`], everything here is expressed against
//! [`Outbox`] alone; the actual round — `/get` list, `/get` fetch, the
//! confirming acknowledgement that alone lets the node delete — is the
//! router's (`request_messages_from_propagation_node`,
//! `leviculum-lxmf/src/router/propagation_runtime.rs`), so this module only
//! sequences it and owns what a *user* sees of it.
//!
//! # What is printed, and what is suppressed
//!
//! Every message is recorded in the [`SeenStore`]; only ids the store has
//! never seen are printed. That is the cross-run half of de-duplication —
//! the router already refuses, within one run, to deliver a message twice
//! (its processed-ids set covers the direct and the mailbox path alike), but
//! a fresh process has a fresh router, and the message that arrived directly
//! yesterday must not reappear because a mailbox still held a copy today.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;
use core::time::Duration;

use leviculum_lxmf::router::PropagationPathAttempts;

use crate::clock::{self, Clock};
use crate::effects::Effects;
use crate::events;
use crate::mailbox_state::{self, MailboxRun};
use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};
use crate::seen::SeenStore;

/// How much of a fetch's budget the drain's path phase leaves to the frontend.
///
/// The engine picks a command up on its next tick and notices the end of the
/// path phase on a later one, each up to `POLL_INTERVAL_MS` (200 ms) late in
/// `engine.rs`. Without a margin the frontend's own clock runs out first and
/// the fetch ends as a bare [`FetchError::Timeout`], which says nothing about
/// the requests the drain sent; with it, a missing path ends as
/// [`FetchError::NoPath`] and is named as such.
const PATH_BUDGET_MARGIN: Duration = Duration::from_secs(1);

/// How long a fetch may take once the daemon connection is up, and how often
/// the event queue is looked at. Same shape as [`crate::send::SendOptions`],
/// separate type so the two commands' defaults can drift apart.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// The shared-instance name, for the log line only.
    pub instance: String,
    /// Total budget: selecting the node, the link, the whole drain.
    pub budget: Duration,
    /// How long to sleep when the event queue is empty.
    pub poll: Duration,
    /// How the mailbox is selected without `--pn`, and where it is kept.
    pub mailbox: MailboxRun,
}

impl FetchOptions {
    pub fn new(instance: impl Into<String>, budget: Duration) -> Self {
        Self {
            instance: instance.into(),
            budget,
            poll: Duration::from_millis(20),
            mailbox: MailboxRun::default(),
        }
    }
}

/// A finished drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetched {
    /// Messages the node handed over in this round.
    pub received: usize,
    /// Of those, how many the router had already processed this run.
    pub duplicates: usize,
    /// Messages printed — new to the user across runs. Can exceed
    /// `received`: a direct delivery arriving mid-fetch is also new mail.
    pub new: usize,
}

/// Why a fetch did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The engine stopped answering.
    Gone,
    /// The engine reported it could not start.
    Broken(String),
    /// No propagation node could be selected.
    NoPropagationNode(String),
    /// The drain started and then failed after its link to the node came up,
    /// in the state's own words.
    SyncFailed(String),
    /// The drain started and the node never answered it: it failed, in the
    /// state's own words, or the budget ran out, before a link came up.
    Unanswered(String),
    /// The drain asked for a path to the node until its budget ended, and
    /// none came.
    NoPath(PropagationPathAttempts),
    /// The budget ran out after the link came up and before the round
    /// completed.
    Timeout,
}

impl FetchError {
    /// The node never answered: no path to it, or no link to it came up.
    /// What counts toward a mailbox being replaced
    /// ([`crate::mailbox::Unanswered`]).
    pub fn before_link(&self) -> bool {
        matches!(self, Self::NoPath(_) | Self::Unanswered(_))
    }
}

impl From<OutboxGone> for FetchError {
    fn from(_: OutboxGone) -> Self {
        Self::Gone
    }
}

impl core::fmt::Display for FetchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Gone => write!(f, "the message engine stopped answering"),
            Self::Broken(detail) => write!(f, "the message engine could not start: {detail}"),
            Self::NoPropagationNode(detail) => write!(
                f,
                "no propagation node: {detail}.\n  \
                 Name one with --pn <hash>, or set propagation_node in Loquax's config."
            ),
            Self::SyncFailed(detail) => write!(f, "checking the propagation node failed: {detail}"),
            Self::Unanswered(detail) => write!(
                f,
                "the mailbox sync failed: {detail}.\n  \
                 Raise --timeout, or check that the node is reachable."
            ),
            Self::NoPath(attempts) => write!(
                f,
                "the mailbox sync failed: no path to the propagation node after {} path \
                 requests over {:.1} s.\n  \
                 Raise --timeout, or check that the node is reachable.",
                attempts.requests,
                attempts.waited_ms as f64 / 1000.0,
            ),
            Self::Timeout => write!(
                f,
                "the mailbox sync did not complete inside the timeout.\n  \
                 Raise --timeout, or check that the node is reachable."
            ),
        }
    }
}

impl core::error::Error for FetchError {}

/// Render one message the way `loquax fetch` shows mail: a single header line
/// a script can key on, the body verbatim, a blank separator. The header
/// carries the message id (the seen-store key, so an operator can connect a
/// suppressed duplicate to its first appearance) and names an unverifiable
/// signature out loud, because a message from an unannounced sender is the
/// one case where "from" is a claim rather than a fact.
///
/// The header is text and is written through `core::fmt::Write`; the body is
/// bytes, forwarded as the sender wrote them (`body::finish`), so the whole
/// message comes back as bytes rather than as a `String` that would have to
/// replace whatever is not UTF-8.
fn render_message(
    message_id: &[u8; 32],
    source: &[u8; 16],
    title: &[u8],
    body: &[u8],
    verified: bool,
) -> Result<Vec<u8>, core::fmt::Error> {
    let title = String::from_utf8_lossy(title);
    let mut out = String::new();
    write!(
        out,
        "message {} from {}",
        crate::address::to_hex(message_id),
        crate::address::to_hex(source),
    )?;
    if !verified {
        write!(out, " (unverified)")?;
    }
    if !title.trim().is_empty() {
        // The title is one line by contract with the header format; anything
        // a sender smuggled in is flattened rather than allowed to fake a
        // second header.
        let flat: String = title
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        write!(out, " title {flat}")?;
    }
    writeln!(out)?;
    let mut rendered = out.into_bytes();
    rendered.extend_from_slice(body);
    rendered.extend_from_slice(b"\n\n");
    Ok(rendered)
}

/// One received message as a fetch takes it: recorded as seen, and printed
/// the first time it is seen. Whether it was new.
pub fn take_received<E: Effects>(
    seen: &mut SeenStore,
    effects: &mut E,
    message_id: &[u8; 32],
    source: &[u8; 16],
    title: &[u8],
    body: &[u8],
    verified: bool,
) -> Result<bool, FetchError> {
    // An unwritable seen record must not cost mail: the worst a failed
    // write can do is a duplicate print next run, and that beats a message
    // nobody saw.
    let first_sight = match seen.record(*message_id) {
        Some(write) => {
            if let Err(error) = effects.write_seen(&write) {
                effects.notice(&format!(
                    "could not record {} as seen: {error}",
                    crate::address::to_hex(message_id)
                ));
            }
            true
        }
        None => false,
    };
    events::fetched(message_id, source, body.len(), !first_sight);
    if first_sight {
        // Rendered into one buffer and printed in one call: the binary's
        // stdout is unlocked and shared with the tracing subscriber's own
        // lines, and a message split across many small writes could have a
        // log line land in the middle of its body.
        let rendered = render_message(message_id, source, title, body, verified)
            .map_err(|error| FetchError::SyncFailed(error.to_string()))?;
        effects.print(&rendered).map_err(FetchError::SyncFailed)?;
    }
    Ok(first_sight)
}

/// Run one `loquax fetch`.
///
/// `preferred_pn` and `pn_source` are as in
/// [`crate::send::run_send_propagated`]. Messages are printed through
/// [`Effects::print`] (stdout in the binary, a buffer in tests); the seen
/// record is written through `effects` as each message is printed, not at
/// the end, so an interrupted drain never re-prints. The mailbox decision is
/// shown and kept through `effects` too. The budget runs on `clock`.
pub async fn run_fetch<O: Outbox, E: Effects>(
    outbox: &mut O,
    preferred_pn: Option<[u8; 16]>,
    pn_source: &str,
    options: &FetchOptions,
    effects: &mut E,
    seen: &mut SeenStore,
    clock: &impl Clock,
) -> Result<Fetched, FetchError> {
    let start = clock.now_ms();
    outbox.submit(Command::SelectPn {
        preferred: preferred_pn,
        mailbox: options.mailbox.input.clone(),
    })?;

    let mut fetching = false;
    let mut linked = false;
    let mut new = 0usize;

    loop {
        while let Some(event) = outbox.try_next_event()? {
            match event {
                OutboxEvent::Ready { address } => {
                    events::attached(&options.instance, &address);
                }
                OutboxEvent::Broken { detail } => return Err(FetchError::Broken(detail)),
                OutboxEvent::PnSelected {
                    destination,
                    stamp_cost,
                } => {
                    if !fetching {
                        events::pn(&destination, pn_source, stamp_cost);
                        if pn_source == "flag" {
                            mailbox_state::record_user_choice(destination, effects);
                        }
                        // What is left of the budget, so the drain asks for a
                        // path for as long as the user said the fetch may take
                        // (leviculum#719), not for a fixed window.
                        let left = options
                            .budget
                            .saturating_sub(clock::since(clock, start))
                            .saturating_sub(PATH_BUDGET_MARGIN);
                        outbox.submit(Command::Fetch {
                            path_budget_ms: u64::try_from(left.as_millis()).unwrap_or(u64::MAX),
                        })?;
                        fetching = true;
                    }
                }
                OutboxEvent::PnUnavailable { detail } => {
                    return Err(FetchError::NoPropagationNode(detail));
                }
                OutboxEvent::Mailbox(decision) => mailbox_state::report(&decision, effects),
                OutboxEvent::Received {
                    message_id,
                    source,
                    title,
                    body,
                    verified,
                    ..
                } => {
                    if take_received(seen, effects, &message_id, &source, &title, &body, verified)?
                    {
                        new += 1;
                    }
                }
                OutboxEvent::SyncDone {
                    received,
                    duplicates,
                } => {
                    events::sync_done(received, duplicates, new);
                    return Ok(Fetched {
                        received,
                        duplicates,
                        new,
                    });
                }
                OutboxEvent::SyncLinked if fetching => linked = true,
                OutboxEvent::SyncFailed {
                    path: Some(attempts),
                    ..
                } if fetching => return Err(FetchError::NoPath(attempts)),
                OutboxEvent::SyncFailed { detail, .. } if fetching && linked => {
                    return Err(FetchError::SyncFailed(detail));
                }
                OutboxEvent::SyncFailed { detail, .. } if fetching => {
                    return Err(FetchError::Unanswered(detail));
                }
                // Send-side traffic; a fetch has none in flight.
                _ => {}
            }
        }

        if clock::since(clock, start) >= options.budget {
            return Err(if linked {
                FetchError::Timeout
            } else if fetching {
                FetchError::Unanswered(
                    "the propagation node did not answer a link inside the timeout".to_string(),
                )
            } else {
                FetchError::NoPropagationNode(
                    "selecting a node did not finish inside the timeout".to_string(),
                )
            });
        }
        clock::sleep(clock, options.poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::clock::fake::Stepping;
    use crate::clock::Tokio;
    use crate::effects::fake::Recorded;
    use crate::outbox::fake::FakeOutbox;
    use crate::seen::SeenWrite;

    const PN: [u8; 16] = [0x11; 16];
    const SRC: [u8; 16] = [0x22; 16];
    const ID_A: [u8; 32] = [0xa1; 32];
    const ID_B: [u8; 32] = [0xb2; 32];

    fn options() -> FetchOptions {
        FetchOptions {
            instance: "test".to_string(),
            budget: Duration::from_secs(5),
            poll: Duration::from_millis(1),
            mailbox: MailboxRun::default(),
        }
    }

    fn received(id: [u8; 32], body: &[u8]) -> OutboxEvent {
        OutboxEvent::Received {
            sent_at: 0,
            message_id: id,
            source: SRC,
            title: b"t".to_vec(),
            body: body.to_vec(),
            fields: Vec::new(),
            verified: true,
        }
    }

    fn effects() -> Recorded {
        Recorded::default()
    }

    /// The genuine round: select, fetch, print, done — and the drain's
    /// counts come back to the caller.
    #[tokio::test(start_paused = true)]
    async fn a_drain_prints_new_messages_and_reports_the_counts() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: [1; 16] },
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: Some(13),
            },
            received(ID_A, b"first body"),
            received(ID_B, b"second body"),
            OutboxEvent::SyncDone {
                received: 2,
                duplicates: 0,
            },
        ]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        let fetched = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect("a clean drain succeeds");

        assert_eq!(
            fetched,
            Fetched {
                received: 2,
                duplicates: 0,
                new: 2
            }
        );
        let text = String::from_utf8(effects.printed).expect("utf8");
        assert!(text.contains("first body") && text.contains("second body"));
        let commands = outbox.commands.borrow();
        assert!(
            matches!(
                commands[0],
                Command::SelectPn {
                    preferred: None,
                    ..
                }
            ),
            "selection first: {commands:?}"
        );
        // The path budget is the whole budget less the margin: the fake
        // clock is paused, so no time has passed.
        assert_eq!(
            commands[1],
            Command::Fetch {
                path_budget_ms: 4_000
            }
        );
    }

    /// The cross-run half of the dedup rule: a message an earlier run
    /// already delivered is recorded but not printed again.
    #[tokio::test(start_paused = true)]
    async fn an_already_seen_message_is_not_printed_again() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            received(ID_A, b"the copy a mailbox still held"),
            received(ID_B, b"genuinely new"),
            OutboxEvent::SyncDone {
                received: 2,
                duplicates: 0,
            },
        ]);
        // What an earlier run left on disk.
        let (mut seen, _) = SeenStore::from_text(&(crate::address::to_hex(&ID_A) + "\n"));
        let mut effects = effects();

        let fetched = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect("dedup is not an error");

        assert_eq!(fetched.new, 1, "only the new message counts");
        let text = String::from_utf8(effects.printed).expect("utf8");
        assert!(!text.contains("the copy a mailbox still held"));
        assert!(text.contains("genuinely new"));
        assert_eq!(
            effects.seen,
            vec![SeenWrite::Append(crate::address::to_hex(&ID_B) + "\n")],
            "only the new id is appended to the record"
        );
    }

    /// The comment in `run_fetch` made true: a seen record that cannot be
    /// written costs a notice and, next run, a duplicate, never this run's
    /// print.
    #[tokio::test(start_paused = true)]
    async fn an_unwritable_seen_record_still_prints_and_says_so() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            received(ID_A, b"must not be lost"),
            OutboxEvent::SyncDone {
                received: 1,
                duplicates: 0,
            },
        ]);
        let mut seen = SeenStore::default();
        let mut effects = Recorded {
            seen_fails: Some("No space left on device"),
            ..Recorded::default()
        };

        let fetched = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect("a failed record is not a failed fetch");

        assert_eq!(fetched.new, 1);
        assert!(String::from_utf8(effects.printed)
            .expect("utf8")
            .contains("must not be lost"));
        assert_eq!(
            effects.notices,
            vec![format!(
                "could not record {} as seen: No space left on device",
                crate::address::to_hex(&ID_A)
            )]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_selectable_node_is_its_own_failure() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::PnUnavailable {
            detail: "none announced".to_string(),
        }]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        let error = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect_err("no node is a failure");
        assert!(
            matches!(error, FetchError::NoPropagationNode(_)),
            "{error:?}"
        );
        assert!(effects.printed.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_sync_reports_the_state_word() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::SyncFailed {
                detail: "the propagation node did not answer".to_string(),
                path: None,
            },
        ]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        let error = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect_err("a failed sync is a failure");
        assert_eq!(
            error,
            FetchError::Unanswered("the propagation node did not answer".to_string())
        );
        assert!(error.before_link());
    }

    /// A failure after the link came up is the node's answer or the
    /// transfer, never a node that stopped answering.
    #[tokio::test(start_paused = true)]
    async fn a_failure_after_the_link_is_not_unanswered() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::SyncLinked,
            OutboxEvent::SyncFailed {
                detail: "the transfer failed".to_string(),
                path: None,
            },
        ]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        let error = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect_err("a failed sync is a failure");
        assert_eq!(
            error,
            FetchError::SyncFailed("the transfer failed".to_string())
        );
        assert!(!error.before_link());
    }

    /// leviculum#719: a missing path names how many requests the drain sent
    /// and over how long, so a red cell does not need its `PKT_TX` counted.
    #[tokio::test(start_paused = true)]
    async fn a_missing_path_names_its_requests() {
        let attempts = PropagationPathAttempts {
            requests: 9,
            waited_ms: 59_000,
        };
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::SyncFailed {
                detail: "no path to the propagation node".to_string(),
                path: Some(attempts),
            },
        ]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        let error = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect_err("no path is a failure");
        assert_eq!(error, FetchError::NoPath(attempts));
        assert!(error.before_link());
        assert!(
            error
                .to_string()
                .contains("after 9 path requests over 59.0 s"),
            "{error}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_drain_that_never_finishes_times_out() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::PnSelected {
            destination: PN,
            stamp_cost: None,
        }]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        let error = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect_err("no SyncDone inside the budget is a failure");
        assert_eq!(
            error,
            FetchError::Unanswered(
                "the propagation node did not answer a link inside the timeout".to_string()
            )
        );
        assert!(error.before_link());
    }

    /// The budget running out once the link is up is a plain timeout: the
    /// node answered.
    #[tokio::test(start_paused = true)]
    async fn a_drain_that_links_and_never_finishes_times_out() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::SyncLinked,
        ]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        let error = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect_err("no SyncDone inside the budget is a failure");
        assert_eq!(error, FetchError::Timeout);
        assert!(!error.before_link());
    }

    /// The budget runs on the clock the fetch is given: on a stepping clock
    /// the path budget handed to the drain is what is left after the sleeps
    /// so far, and the 5 s budget at a 1 ms poll ends after 5 000 sleeps.
    #[tokio::test]
    async fn the_budget_is_measured_on_the_clock_it_is_given() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::PnSelected {
            destination: PN,
            stamp_cost: None,
        }]);
        let mut seen = SeenStore::default();
        let mut effects = effects();
        let clock = Stepping::default();

        let error = run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &clock,
        )
        .await
        .expect_err("no SyncDone inside the budget is a failure");
        assert!(error.before_link(), "{error:?}");
        assert_eq!(clock.sleeps.get(), 5_000);
        assert_eq!(
            outbox.commands.borrow()[1],
            Command::Fetch {
                path_budget_ms: 4_000
            }
        );
    }

    /// A body is bytes and is printed as such: what is not UTF-8 reaches
    /// stdout unchanged, not replaced.
    #[tokio::test(start_paused = true)]
    async fn a_body_that_is_not_utf8_is_printed_byte_for_byte() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            received(ID_A, b"\xff\xfe raw"),
            OutboxEvent::SyncDone {
                received: 1,
                duplicates: 0,
            },
        ]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect("a binary body still arrives");
        let header = format!(
            "message {} from {} title t\n",
            crate::address::to_hex(&ID_A),
            crate::address::to_hex(&SRC)
        );
        let mut expected = header.into_bytes();
        expected.extend_from_slice(b"\xff\xfe raw\n\n");
        assert_eq!(effects.printed, expected);
    }

    /// An unverified signature is said out loud in the one line a reader
    /// certainly sees.
    #[tokio::test(start_paused = true)]
    async fn an_unverified_sender_is_named_in_the_header() {
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID_A,
                source: SRC,
                title: Vec::new(),
                body: b"body".to_vec(),
                fields: Vec::new(),
                verified: false,
            },
            OutboxEvent::SyncDone {
                received: 1,
                duplicates: 0,
            },
        ]);
        let mut seen = SeenStore::default();
        let mut effects = effects();

        run_fetch(
            &mut outbox,
            None,
            "adopted",
            &options(),
            &mut effects,
            &mut seen,
            &Tokio::new(),
        )
        .await
        .expect("an unverified message still arrives");
        let text = String::from_utf8(effects.printed).expect("utf8");
        assert!(text.contains("(unverified)"), "{text}");
    }
}
