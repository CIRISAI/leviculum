//! The frontend of `lnmsg fetch`: drain the selected propagation node's
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

use std::io::Write;
use std::time::Duration;

use tokio::time::Instant;

use crate::events;
use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};
use crate::seen::SeenStore;

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
}

impl FetchOptions {
    pub fn new(instance: impl Into<String>, budget: Duration) -> Self {
        Self {
            instance: instance.into(),
            budget,
            poll: Duration::from_millis(20),
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
    /// The drain started and then failed, in the state's own words.
    SyncFailed(String),
    /// The budget ran out before the round completed.
    Timeout,
}

impl From<OutboxGone> for FetchError {
    fn from(_: OutboxGone) -> Self {
        Self::Gone
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => write!(f, "the message engine stopped answering"),
            Self::Broken(detail) => write!(f, "the message engine could not start: {detail}"),
            Self::NoPropagationNode(detail) => write!(
                f,
                "no propagation node: {detail}.\n  \
                 Name one with --pn <hash>, or set propagation_node in lnmsg's config."
            ),
            Self::SyncFailed(detail) => write!(f, "the mailbox sync failed: {detail}"),
            Self::Timeout => write!(
                f,
                "the mailbox sync did not complete inside the timeout.\n  \
                 Raise --timeout, or check that the node is reachable."
            ),
        }
    }
}

impl std::error::Error for FetchError {}

/// Print one message the way `lnmsg fetch` shows mail: a single header line
/// a script can key on, the body verbatim, a blank separator. The header
/// carries the message id (the seen-store key, so an operator can connect a
/// suppressed duplicate to its first appearance) and names an unverifiable
/// signature out loud, because a message from an unannounced sender is the
/// one case where "from" is a claim rather than a fact.
fn print_message(
    out: &mut impl Write,
    message_id: &[u8; 32],
    source: &[u8; 16],
    title: &[u8],
    body: &[u8],
    verified: bool,
) -> std::io::Result<()> {
    let title = String::from_utf8_lossy(title);
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
    out.write_all(body)?;
    writeln!(out)?;
    writeln!(out)
}

/// Run one `lnmsg fetch`.
///
/// `preferred_pn` and `pn_source` are as in
/// [`crate::send::run_send_propagated`]. Messages go to `out` (stdout in the
/// binary, a buffer in tests); the seen store is updated as each message is
/// printed, not at the end, so an interrupted drain never re-prints.
pub async fn run_fetch<O: Outbox, W: Write>(
    outbox: &mut O,
    preferred_pn: Option<[u8; 16]>,
    pn_source: &str,
    options: &FetchOptions,
    seen: &mut SeenStore,
    out: &mut W,
) -> Result<Fetched, FetchError> {
    let start = Instant::now();
    outbox.submit(Command::SelectPn {
        preferred: preferred_pn,
    })?;

    let mut fetching = false;
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
                        outbox.submit(Command::Fetch)?;
                        fetching = true;
                    }
                }
                OutboxEvent::PnUnavailable { detail } => {
                    return Err(FetchError::NoPropagationNode(detail));
                }
                OutboxEvent::Received {
                    message_id,
                    source,
                    title,
                    body,
                    verified,
                } => {
                    // An unwritable seen store must not cost mail: the worst
                    // a failed record can do is a duplicate print next run,
                    // and that beats a message nobody saw.
                    let first_sight = match seen.record(message_id) {
                        Ok(first_sight) => first_sight,
                        Err(error) => {
                            eprintln!("lnmsg: could not record {} as seen: {error}", {
                                crate::address::to_hex(&message_id)
                            });
                            !seen.contains(&message_id)
                        }
                    };
                    events::fetched(&message_id, &source, body.len(), !first_sight);
                    if first_sight {
                        new += 1;
                        // Rendered into one buffer and written in one call:
                        // `out` is an unlocked stdout in the binary, shared
                        // with the tracing subscriber's own lines, and a
                        // message split across many small writes could have
                        // a log line land in the middle of its body.
                        let mut rendered = Vec::new();
                        print_message(&mut rendered, &message_id, &source, &title, &body, verified)
                            .map_err(|error| FetchError::SyncFailed(error.to_string()))?;
                        out.write_all(&rendered)
                            .and_then(|()| out.flush())
                            .map_err(|error| FetchError::SyncFailed(error.to_string()))?;
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
                OutboxEvent::SyncFailed { detail } if fetching => {
                    return Err(FetchError::SyncFailed(detail));
                }
                // Send-side traffic; a fetch has none in flight.
                _ => {}
            }
        }

        if start.elapsed() >= options.budget {
            return Err(if fetching {
                FetchError::Timeout
            } else {
                FetchError::NoPropagationNode(
                    "selecting a node did not finish inside the timeout".to_string(),
                )
            });
        }
        tokio::time::sleep(options.poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::outbox::fake::FakeOutbox;

    const PN: [u8; 16] = [0x11; 16];
    const SRC: [u8; 16] = [0x22; 16];
    const ID_A: [u8; 32] = [0xa1; 32];
    const ID_B: [u8; 32] = [0xb2; 32];

    fn options() -> FetchOptions {
        FetchOptions {
            instance: "test".to_string(),
            budget: Duration::from_secs(5),
            poll: Duration::from_millis(1),
        }
    }

    fn received(id: [u8; 32], body: &[u8]) -> OutboxEvent {
        OutboxEvent::Received {
            message_id: id,
            source: SRC,
            title: b"t".to_vec(),
            body: body.to_vec(),
            verified: true,
        }
    }

    fn store(dir: &std::path::Path) -> SeenStore {
        SeenStore::load(dir).expect("seen store")
    }

    /// The genuine round: select, fetch, print, done — and the drain's
    /// counts come back to the caller.
    #[tokio::test(start_paused = true)]
    async fn a_drain_prints_new_messages_and_reports_the_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
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
        let mut seen = store(dir.path());
        let mut printed = Vec::new();

        let fetched = run_fetch(
            &mut outbox,
            None,
            "announced",
            &options(),
            &mut seen,
            &mut printed,
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
        let text = String::from_utf8(printed).expect("utf8");
        assert!(text.contains("first body") && text.contains("second body"));
        let commands = outbox.commands.borrow();
        assert!(
            matches!(commands[0], Command::SelectPn { preferred: None }),
            "selection first: {commands:?}"
        );
        assert!(matches!(commands[1], Command::Fetch));
    }

    /// The cross-run half of the dedup rule: a message an earlier run
    /// already delivered is recorded but not printed again.
    #[tokio::test(start_paused = true)]
    async fn an_already_seen_message_is_not_printed_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut earlier = store(dir.path());
            earlier.record(ID_A).expect("record in the earlier run");
        }
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
        let mut seen = store(dir.path());
        let mut printed = Vec::new();

        let fetched = run_fetch(
            &mut outbox,
            None,
            "announced",
            &options(),
            &mut seen,
            &mut printed,
        )
        .await
        .expect("dedup is not an error");

        assert_eq!(fetched.new, 1, "only the new message counts");
        let text = String::from_utf8(printed).expect("utf8");
        assert!(!text.contains("the copy a mailbox still held"));
        assert!(text.contains("genuinely new"));
    }

    #[tokio::test(start_paused = true)]
    async fn no_selectable_node_is_its_own_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::PnUnavailable {
            detail: "none announced".to_string(),
        }]);
        let mut seen = store(dir.path());
        let mut printed = Vec::new();

        let error = run_fetch(
            &mut outbox,
            None,
            "announced",
            &options(),
            &mut seen,
            &mut printed,
        )
        .await
        .expect_err("no node is a failure");
        assert!(
            matches!(error, FetchError::NoPropagationNode(_)),
            "{error:?}"
        );
        assert!(printed.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_sync_reports_the_state_word() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::SyncFailed {
                detail: "the propagation node did not answer".to_string(),
            },
        ]);
        let mut seen = store(dir.path());
        let mut printed = Vec::new();

        let error = run_fetch(
            &mut outbox,
            None,
            "announced",
            &options(),
            &mut seen,
            &mut printed,
        )
        .await
        .expect_err("a failed sync is a failure");
        assert_eq!(
            error,
            FetchError::SyncFailed("the propagation node did not answer".to_string())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_drain_that_never_finishes_times_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::PnSelected {
            destination: PN,
            stamp_cost: None,
        }]);
        let mut seen = store(dir.path());
        let mut printed = Vec::new();

        let error = run_fetch(
            &mut outbox,
            None,
            "announced",
            &options(),
            &mut seen,
            &mut printed,
        )
        .await
        .expect_err("no SyncDone inside the budget is a failure");
        assert_eq!(error, FetchError::Timeout);
    }

    /// An unverified signature is said out loud in the one line a reader
    /// certainly sees.
    #[tokio::test(start_paused = true)]
    async fn an_unverified_sender_is_named_in_the_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut outbox = FakeOutbox::new(vec![
            OutboxEvent::PnSelected {
                destination: PN,
                stamp_cost: None,
            },
            OutboxEvent::Received {
                message_id: ID_A,
                source: SRC,
                title: Vec::new(),
                body: b"body".to_vec(),
                verified: false,
            },
            OutboxEvent::SyncDone {
                received: 1,
                duplicates: 0,
            },
        ]);
        let mut seen = store(dir.path());
        let mut printed = Vec::new();

        run_fetch(
            &mut outbox,
            None,
            "announced",
            &options(),
            &mut seen,
            &mut printed,
        )
        .await
        .expect("an unverified message still arrives");
        let text = String::from_utf8(printed).expect("utf8");
        assert!(text.contains("(unverified)"), "{text}");
    }
}
