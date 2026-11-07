//! A client's ask of its home node without the waiting: the path ladder,
//! the link's budget and an ask's steps (`/join`, `/sync` page by page,
//! `/send`, `/mark`, and a group's `/group`, `/groupadd`, `/groupdelete`
//! and state message), `no_std`, so the M9 runs the same flow the
//! desktop and the phone run (`handheld-messenger.md`, "Networking: a
//! client of a home, over LoRa"; 1359, 1363).
//!
//! The platform owns the clock and the link API and asks this module what
//! to do next. `client` drives it over `leviculum_std::ReticulumNode` and
//! tokio; the board will drive it over `leviculum-core`'s links and the
//! firmware's clock. Every time is a millisecond count of the platform's
//! own monotonic clock.

use alloc::string::String;
use alloc::vec::Vec;

use super::code::HomeCode;
use super::wire::{self, Status};

/// How long after a path request the next one goes while no path has come:
/// first the reference's wait between a path request and the next attempt
/// (`leviculum_lxmf::router::PATH_REQUEST_WAIT_MS`), doubled on each one
/// unanswered up to [`PATH_RETRY_MAX_MS`]. A mailbox behind a third party's
/// plain port whose announce has not reached the entry node yet is found by
/// a later request, or by its next announce (1196).
pub const PATH_RETRY_MS: u64 = leviculum_lxmf::router::PATH_REQUEST_WAIT_MS;
/// The longest wait between two path requests within one attempt.
pub const PATH_RETRY_MAX_MS: u64 = 60_000;

/// Why talking to the home node failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeError {
    /// No path to the home destination came within the budget.
    NoPath,
    /// The link did not come up.
    NoLink(String),
    /// The home node answered with a refusal.
    Refused(Status),
    /// The request or its answer did not make it.
    Transfer(String),
    /// The answer did not decode.
    Malformed,
    /// The budget ran out.
    Timeout,
    /// The home node answers on the link but stays silent on this path:
    /// it was built before the path (a home before 1307 drops `/group`,
    /// no handler).
    Unsupported(&'static str),
}

impl core::fmt::Display for HomeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoPath => write!(f, "no path to the mailbox"),
            Self::NoLink(detail) => write!(f, "the mailbox did not answer the link: {detail}"),
            Self::Refused(status) => write!(f, "{status}"),
            Self::Transfer(detail) => write!(f, "the request to the mailbox failed: {detail}"),
            Self::Malformed => write!(f, "the mailbox's answer did not decode"),
            Self::Timeout => write!(f, "the mailbox did not answer in time"),
            Self::Unsupported(path) => write!(f, "the mailbox does not know {path}"),
        }
    }
}

/// When the path requests of one attempt go: the first at once, the next
/// [`PATH_RETRY_MS`] after it, each later one after twice the wait before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathLadder {
    asked_at: Option<u64>,
    retry_ms: u64,
    requests: u32,
}

impl Default for PathLadder {
    fn default() -> Self {
        Self::new()
    }
}

impl PathLadder {
    /// A ladder with no request asked yet.
    pub const fn new() -> Self {
        Self {
            asked_at: None,
            retry_ms: PATH_RETRY_MS,
            requests: 0,
        }
    }

    /// Whether a path request goes at `now_ms`; counted when it does.
    pub fn due(&mut self, now_ms: u64) -> bool {
        if let Some(at) = self.asked_at {
            if now_ms.saturating_sub(at) >= self.retry_ms {
                self.retry_ms = self.retry_ms.saturating_mul(2).min(PATH_RETRY_MAX_MS);
                self.asked_at = None;
            }
        }
        if self.asked_at.is_some() {
            return false;
        }
        self.asked_at = Some(now_ms);
        self.requests = self.requests.saturating_add(1);
        true
    }

    /// How many path requests went.
    pub fn requests(&self) -> u32 {
        self.requests
    }

    /// The wait before the next request, in milliseconds.
    pub fn retry_ms(&self) -> u64 {
        self.retry_ms
    }
}

/// What a link over `hops` hops may take to come up, whatever the caller's
/// budget: the transport's own establishment time per hop as the reference
/// reckons it (`ESTABLISHMENT_TIMEOUT_PER_HOP`,
/// `reference/Reticulum/RNS/Link.py:284`), rule 3 of 1196. A path of no
/// hops counts as one.
pub fn establishment_ms(hops: u8) -> u64 {
    leviculum_core::constants::ESTABLISHMENT_TIMEOUT_PER_HOP_MS
        .saturating_mul(u64::from(hops.max(1)))
}

/// What a `/join` answer means.
pub fn join_outcome(answer: &[u8]) -> Result<(), HomeError> {
    match wire::read_status(answer).map_err(|_| HomeError::Malformed)? {
        Status::Ok => Ok(()),
        status => Err(HomeError::Refused(status)),
    }
}

/// The link MDU on a link of the default MTU (500), which a LoRa path
/// keeps: `Link::mdu` (`leviculum-core/src/link/mod.rs`, 431 at MTU 500,
/// `RNS/Link.py`'s `update_mdu`). A response longer than this goes as a
/// resource (`Node::send_response`, `leviculum-core/src/node/mod.rs`).
pub const LORA_LINK_MDU: usize = 431;

/// The `/sync` page a client on LoRa asks (1363): the longest answer that
/// still travels as one response packet on [`LORA_LINK_MDU`], after the
/// response frame's own array header and the request id's `bin8` (1 + 18
/// bytes). One packet is at most two LoRa frames; a resource is an
/// advertisement, its parts and a proof.
pub const LORA_PAGE_BYTES: usize = LORA_LINK_MDU - RESPONSE_OVERHEAD;

/// The link MDU on a serial line's link, MTU 564 (`board::SERIAL_LINK_MTU`):
/// `Link::mdu`'s rule (`compute_link_mdu`, `leviculum-core/src/link/mod.rs`)
/// at that MTU, 564 less IFAC, header and token (1 + 19 + 48) is 496, cut
/// to whole AES blocks, less one.
pub const SERIAL_LINK_MDU: usize = 495;

/// What a response packet takes of a link's MDU besides the answer: the
/// response frame's array header and the request id's `bin8`.
pub const RESPONSE_OVERHEAD: usize = 1 + 18;

/// How often a request whose answer was lost is asked, on a fresh link
/// each time, when asking twice changes nothing (as `client`'s
/// `answer_retrying`).
pub const ATTEMPTS: u8 = 3;

/// How often a request whose answer did not come in its time is asked
/// again on the same link while that link stays up (1440): a frame the
/// line lost, not a link that broke. Counted per request: a page that
/// arrives starts the count again.
pub const REPEATS: u8 = 3;

/// What a client asks its home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    /// `/join`: redeem the admission token as the device called `name`.
    /// Asked once: the token is single-use, a second ask is refused.
    Join { token: [u8; 16], name: String },
    /// `/sync` from `cursor` until no more follow, each page at most
    /// `page_bytes` when set ([`LORA_PAGE_BYTES`] on LoRa).
    Pull {
        cursor: u64,
        page_bytes: Option<u64>,
    },
    /// `/send` one message.
    Send(wire::Submission),
    /// `/mark` messages read, deleted or seen.
    Mark(wire::Mark),
    /// A group's verb: `/group`, `/groupadd`, `/groupdelete`, or a
    /// member's state sent to the group. Asked again as a send is; a home
    /// before 1307 that drops `/group` ends in [`HomeError::Timeout`]
    /// (`client` probes for that on the desktop, the board does not).
    Group(GroupVerb),
}

/// What a client asks its home about a group (1366): the admin's three
/// requests, and a member's Join, Leave or the admin's Remove, which are
/// a state message to the group's address handed to the home as any
/// message is (`UiModel::group_state`). The verbs the model's group
/// effects become on a client without the desktop's executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupVerb {
    /// `/group`: make a group the home hosts and invite its members.
    Create(wire::GroupRequest),
    /// `/groupadd`: the admin's home adds members to its group.
    Add(wire::GroupAddRequest),
    /// `/groupdelete`: the admin's home deletes its group for everyone.
    Delete(wire::GroupDeleteRequest),
    /// `/send` of a state message to the group: the Join's mark alone, or
    /// the membership naming the member who leaves or is removed.
    State(wire::Submission),
}

impl Verb {
    /// Whether asking again after a lost answer changes nothing.
    fn idempotent(&self) -> bool {
        !matches!(self, Self::Join { .. })
    }
}

/// What a finished [`Ask`] brought.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The device is admitted.
    Joined,
    /// The pull is through: `cursor` the newest sequence taken (the one
    /// asked from when nothing came), `first` the oldest the home holds,
    /// `entries` how many came, in `pages` pages.
    Pulled {
        cursor: u64,
        first: u64,
        entries: usize,
        pages: usize,
    },
    /// The home took the message; its id.
    Sent([u8; 32]),
    /// The home keeps the mark.
    Marked,
    /// The group's address: the one made, added to or deleted.
    Group([u8; 16]),
}

/// An answer that is a value or the status that refused it.
fn refused<T>(read: Result<Result<T, Status>, wire::Malformed>) -> Result<T, HomeError> {
    match read {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(status)) => Err(HomeError::Refused(status)),
        Err(_) => Err(HomeError::Malformed),
    }
}

/// What the platform does next for an [`Ask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing: poll again later.
    Wait,
    /// Ask the network for a path to this destination.
    RequestPath([u8; 16]),
    /// Open a link to the home destination, checked against the home's
    /// signing key; then call [`Ask::linked`].
    Connect {
        destination: [u8; 16],
        signing_key: [u8; 32],
    },
    /// Identify on the link with the device's own identity; then call
    /// [`Ask::identified`].
    Identify,
    /// Send this request on the link; then call [`Ask::answered`].
    Request { path: &'static str, data: Vec<u8> },
    /// A page of a pull: keep its entries, move the cursor past them once
    /// kept, then poll. The ask holds no page it handed out, so a pull's
    /// memory is one page however long the archive.
    Page(wire::Page),
    /// The answer was lost: close the link, then poll; the same request
    /// goes again on a fresh one.
    Close,
    /// The ask is over: close the link if one was opened.
    Done(Result<Outcome, HomeError>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Path,
    Linking,
    Identifying,
    Asking,
    /// A page went out and the next is owed.
    Paged,
    Done(Result<Outcome, HomeError>),
}

/// One ask of a client's home without the waiting: a path to the home
/// destination, a link, the device identified on it, the request (a
/// pull's pages one after another on the same link), the answer read.
/// The desktop drives it over `leviculum_std` (`client::run_ask`), the M9
/// over `leviculum-core` and its executor.
#[derive(Debug, Clone)]
pub struct Ask {
    destination: [u8; 16],
    signing_key: [u8; 32],
    verb: Verb,
    deadline_ms: u64,
    ladder: PathLadder,
    state: State,
    attempts: u8,
    /// Times the request in flight was asked again on its link.
    repeats: u8,
    first: u64,
    entries: usize,
    pages: usize,
    /// What the device folds, said with every `/sync` of a paged pull
    /// (1439).
    fold: wire::Fold,
}

impl Ask {
    /// `verb` asked of the home whose identity's public key this is, begun
    /// at `now_ms` with `budget_ms` to finish in.
    pub fn new(
        public_key: &[u8; 64],
        verb: Verb,
        now_ms: u64,
        budget_ms: u64,
    ) -> Result<Self, HomeError> {
        let destination = super::destination_hash(public_key).map_err(HomeError::NoLink)?;
        let mut signing_key = [0u8; 32];
        signing_key.copy_from_slice(&public_key[32..]);
        Ok(Self {
            destination,
            signing_key,
            verb,
            deadline_ms: now_ms.saturating_add(budget_ms),
            ladder: PathLadder::new(),
            state: State::Path,
            attempts: 1,
            repeats: 0,
            first: 0,
            entries: 0,
            pages: 0,
            fold: wire::Fold::All,
        })
    }

    /// The same ask from a device that folds as `fold` (1439): a paged
    /// pull says it, and the home serves only what the device keeps.
    pub fn folding(mut self, fold: wire::Fold) -> Self {
        self.fold = fold;
        self
    }

    /// Size a pull's pages to the link it got (1439,
    /// `board::LinkClass::page_bytes`): set before the first `/sync`, so
    /// unlike [`Self::refit`] it may grow the page. Anything but a paged
    /// pull is left as it is.
    pub fn fit_link(&mut self, page_bytes: u64) {
        if let Verb::Pull {
            page_bytes: Some(asked),
            ..
        } = &mut self.verb
        {
            *asked = page_bytes;
        }
    }

    /// A join of `code` as the device called `name`.
    pub fn join(
        code: &HomeCode,
        name: &str,
        now_ms: u64,
        budget_ms: u64,
    ) -> Result<Self, HomeError> {
        let verb = Verb::Join {
            token: code.token,
            name: String::from(name),
        };
        Self::new(&code.public_key, verb, now_ms, budget_ms)
    }

    /// The home destination the ask links to.
    pub fn destination(&self) -> [u8; 16] {
        self.destination
    }

    /// How many path requests went.
    pub fn path_requests(&self) -> u32 {
        self.ladder.requests()
    }

    /// How many times the request went, on as many links.
    pub fn attempts(&self) -> u8 {
        self.attempts
    }

    /// What is left of the budget at `now_ms`: how long the platform may
    /// wait on a link or an answer before it polls again.
    pub fn left_ms(&self, now_ms: u64) -> u64 {
        self.deadline_ms.saturating_sub(now_ms)
    }

    /// The next step at `now_ms`, with `path` the hop count of the path to
    /// [`Ask::destination`] when the platform has one.
    pub fn poll(&mut self, now_ms: u64, path: Option<u8>) -> Step {
        if let State::Done(result) = &self.state {
            return Step::Done(result.clone());
        }
        if now_ms >= self.deadline_ms {
            let error = match self.state {
                State::Path => HomeError::NoPath,
                _ => HomeError::Timeout,
            };
            return self.finish(Err(error));
        }
        match self.state {
            State::Path => {}
            State::Paged => {
                self.state = State::Asking;
                return self.request();
            }
            _ => return Step::Wait,
        }
        match path {
            Some(hops) => {
                self.deadline_ms = self
                    .deadline_ms
                    .max(now_ms.saturating_add(establishment_ms(hops)));
                self.state = State::Linking;
                Step::Connect {
                    destination: self.destination,
                    signing_key: self.signing_key,
                }
            }
            None if self.ladder.due(now_ms) => Step::RequestPath(self.destination),
            None => Step::Wait,
        }
    }

    /// The link came up, or did not.
    pub fn linked(&mut self, result: Result<(), String>) -> Step {
        match (&self.state, result) {
            (State::Linking, Ok(())) => {
                self.state = State::Identifying;
                Step::Identify
            }
            (State::Linking, Err(detail)) => self.finish(Err(HomeError::NoLink(detail))),
            _ => self.out_of_turn(),
        }
    }

    /// The device identified on the link, or did not.
    pub fn identified(&mut self, result: Result<(), String>) -> Step {
        match (&self.state, result) {
            (State::Identifying, Ok(())) => {
                self.state = State::Asking;
                self.request()
            }
            (State::Identifying, Err(detail)) => self.finish(Err(HomeError::NoLink(detail))),
            _ => self.out_of_turn(),
        }
    }

    /// The answer to the request came, or the request failed.
    pub fn answered(&mut self, result: Result<&[u8], HomeError>) -> Step {
        match (&self.state, result) {
            (State::Asking, Ok(answer)) => self.read(answer),
            (State::Asking, Err(HomeError::Transfer(_) | HomeError::Timeout))
                if self.verb.idempotent() && self.attempts < ATTEMPTS =>
            {
                self.attempts += 1;
                self.state = State::Path;
                Step::Close
            }
            (State::Asking, Err(error)) => self.finish(Err(error)),
            _ => self.out_of_turn(),
        }
    }

    /// The answer to the request did not come in its time, on a link that
    /// is still up (1440): the line lost a frame of it, so the same
    /// request goes again on the same link, up to [`REPEATS`] times. A
    /// pull's request asks from its cursor and says its page, so a repeat
    /// brings the same page and a late first answer is dropped by its
    /// request id. Past the repeats, or for a verb that must not be asked
    /// twice, it is what [`Self::answered`] makes of a timeout.
    pub fn lost(&mut self) -> Step {
        if self.state == State::Asking && self.verb.idempotent() && self.repeats < REPEATS {
            self.repeats += 1;
            return self.request();
        }
        self.answered(Err(HomeError::Timeout))
    }

    /// Whether the request may be asked again on its link
    /// ([`Self::lost`]): every verb but the join, whose token is spent by
    /// the first ask that reaches the home.
    pub fn repeatable(&self) -> bool {
        self.verb.idempotent()
    }

    /// The answer to a pull's page came larger than the device takes: ask
    /// the same page again, at most `page_bytes` (1414). Only a smaller
    /// page than the one asked; a page that cannot shrink any more ends the
    /// pull with a [`HomeError::Transfer`], never a request that asks for
    /// the same answer again.
    pub fn refit(&mut self, page_bytes: u64) -> Step {
        if self.state != State::Asking {
            return self.out_of_turn();
        }
        match &mut self.verb {
            Verb::Pull {
                page_bytes: Some(asked),
                ..
            } if page_bytes < *asked => {
                *asked = page_bytes;
                self.request()
            }
            Verb::Pull { .. } => self.finish(Err(HomeError::Transfer(String::from(
                "a page larger than this device takes",
            )))),
            _ => self.out_of_turn(),
        }
    }

    /// The page a pull asks now, when it asks one.
    pub fn page_bytes(&self) -> Option<u64> {
        match &self.verb {
            Verb::Pull { page_bytes, .. } => *page_bytes,
            _ => None,
        }
    }

    /// Where a pull stands: the newest sequence taken so far, which a
    /// pull asked again after a broken link starts from.
    pub fn cursor(&self) -> Option<u64> {
        match &self.verb {
            Verb::Pull { cursor, .. } => Some(*cursor),
            _ => None,
        }
    }

    /// The request the verb makes; a pull's from where it stands.
    fn request(&self) -> Step {
        let (path, data) = match &self.verb {
            Verb::Join { token, name } => (super::JOIN_PATH, wire::join_request(token, name)),
            Verb::Pull {
                cursor,
                page_bytes: Some(page),
            } => (
                super::SYNC_PATH,
                wire::sync_request_folded(*cursor, *page, self.fold),
            ),
            Verb::Pull {
                cursor,
                page_bytes: None,
            } => (super::SYNC_PATH, wire::sync_request(*cursor)),
            Verb::Send(submission) => (super::SEND_PATH, wire::send_request(submission)),
            Verb::Mark(mark) => (super::MARK_PATH, wire::mark_request(mark)),
            Verb::Group(GroupVerb::Create(group)) => {
                (super::GROUP_PATH, wire::group_request(group))
            }
            Verb::Group(GroupVerb::Add(add)) => {
                (super::GROUP_ADD_PATH, wire::group_add_request(add))
            }
            Verb::Group(GroupVerb::Delete(delete)) => {
                (super::GROUP_DELETE_PATH, wire::group_delete_request(delete))
            }
            Verb::Group(GroupVerb::State(state)) => (super::SEND_PATH, wire::send_request(state)),
        };
        Step::Request { path, data }
    }

    /// What the home's answer says, for the verb asked.
    fn read(&mut self, answer: &[u8]) -> Step {
        let outcome = match &mut self.verb {
            Verb::Join { .. } => join_outcome(answer).map(|()| Outcome::Joined),
            Verb::Send(_) => refused(wire::read_send_answer(answer)).map(Outcome::Sent),
            Verb::Mark(_) => join_outcome(answer).map(|()| Outcome::Marked),
            Verb::Group(GroupVerb::Create(_) | GroupVerb::Add(_)) => {
                refused(wire::read_group_answer(answer)).map(Outcome::Group)
            }
            Verb::Group(GroupVerb::Delete(delete)) => {
                let group = delete.group;
                join_outcome(answer).map(|()| Outcome::Group(group))
            }
            Verb::Group(GroupVerb::State(_)) => {
                refused(wire::read_send_answer(answer)).map(Outcome::Sent)
            }
            Verb::Pull { cursor, .. } => {
                let page = match refused(wire::read_sync_answer(answer)) {
                    Ok(page) => page,
                    Err(error) => return self.finish(Err(error)),
                };
                self.first = page.first;
                self.repeats = 0;
                self.entries += page.entries.len();
                self.pages += 1;
                let last = page.entries.last().map(|entry| entry.seq);
                let more = matches!((page.more, last), (true, Some(last)) if last > *cursor);
                if let Some(last) = last {
                    *cursor = (*cursor).max(last);
                }
                self.state = match more {
                    true => State::Paged,
                    false => State::Done(Ok(Outcome::Pulled {
                        cursor: *cursor,
                        first: self.first,
                        entries: self.entries,
                        pages: self.pages,
                    })),
                };
                return Step::Page(page);
            }
        };
        self.finish(outcome)
    }

    fn finish(&mut self, result: Result<Outcome, HomeError>) -> Step {
        self.state = State::Done(result.clone());
        Step::Done(result)
    }

    /// A report the flow did not ask for: a platform bug, ended rather
    /// than followed.
    fn out_of_turn(&mut self) -> Step {
        match &self.state {
            State::Done(result) => Step::Done(result.clone()),
            _ => self.finish(Err(HomeError::Transfer(String::from(
                "a step reported out of turn",
            )))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::Identity;

    fn code() -> (Identity, HomeCode) {
        let home = Identity::generate(&mut rand_core::OsRng);
        let code = HomeCode {
            public_key: home.public_key_bytes(),
            token: [7; 16],
        };
        (home, code)
    }

    /// Requests at 0, 7 s, 21 s, 49 s, 105 s, then a minute apart: the
    /// wait doubles from the reference's 7 s up to the minute.
    #[test]
    fn the_ladder_doubles_its_wait_up_to_the_minute() {
        let mut ladder = PathLadder::new();
        let asked: Vec<u64> = (0..=300_000u64)
            .step_by(100)
            .filter(|&now| ladder.due(now))
            .collect();
        assert_eq!(
            asked,
            [0, 7_000, 21_000, 49_000, 105_000, 165_000, 225_000, 285_000]
        );
        assert_eq!(ladder.requests(), 8);
        assert_eq!(ladder.retry_ms(), PATH_RETRY_MAX_MS);
    }

    /// The whole join: path requests while no path is known, the link,
    /// the identify, `/join` with the code's token, and an Ok answer.
    #[test]
    fn a_join_walks_path_link_identify_request_and_reads_ok() {
        let (_, code) = code();
        let mut join = Ask::join(&code, "M9", 1_000, 60_000).expect("join");
        let destination = join.destination();
        assert_eq!(join.poll(1_000, None), Step::RequestPath(destination));
        assert_eq!(join.poll(1_100, None), Step::Wait);
        assert_eq!(join.poll(8_000, None), Step::RequestPath(destination));
        assert_eq!(
            join.poll(9_000, Some(3)),
            Step::Connect {
                destination,
                signing_key: code.public_key[32..].try_into().expect("32"),
            }
        );
        assert_eq!(join.poll(9_100, Some(3)), Step::Wait);
        assert_eq!(join.linked(Ok(())), Step::Identify);
        match join.identified(Ok(())) {
            Step::Request { path, data } => {
                assert_eq!(path, super::super::JOIN_PATH);
                assert_eq!(data, wire::join_request(&code.token, "M9"));
            }
            other => panic!("not the request: {other:?}"),
        }
        let ok = wire::status_answer(Status::Ok);
        assert_eq!(join.answered(Ok(&ok)), Step::Done(Ok(Outcome::Joined)));
        assert_eq!(join.poll(9_200, Some(3)), Step::Done(Ok(Outcome::Joined)));
        assert_eq!(join.path_requests(), 2);
    }

    /// A refusal is the home's status; no path in the budget is `NoPath`;
    /// a link over many hops gets the transport's time past the budget.
    #[test]
    fn a_refusal_no_path_and_a_long_link_end_as_they_should() {
        let (_, code) = code();
        let mut join = Ask::join(&code, "M9", 0, 10_000).expect("join");
        join.poll(0, Some(1));
        join.linked(Ok(()));
        join.identified(Ok(()));
        let used = wire::status_answer(Status::BadToken);
        assert_eq!(
            join.answered(Ok(&used)),
            Step::Done(Err(HomeError::Refused(Status::BadToken)))
        );

        let mut join = Ask::join(&code, "M9", 0, 10_000).expect("join");
        assert_eq!(
            join.poll(9_999, None),
            Step::RequestPath(join.destination())
        );
        assert_eq!(join.poll(10_000, None), Step::Done(Err(HomeError::NoPath)));

        let mut join = Ask::join(&code, "M9", 0, 10_000).expect("join");
        assert!(matches!(join.poll(9_000, Some(4)), Step::Connect { .. }));
        let past = 9_000 + establishment_ms(4) - 1;
        assert_eq!(join.poll(past, Some(4)), Step::Wait);
        assert_eq!(
            join.poll(past + 1, Some(4)),
            Step::Done(Err(HomeError::Timeout))
        );
    }

    /// The positive control for the order: an answer before the request
    /// went ends the join instead of reading it as the home's word.
    #[test]
    fn a_report_out_of_turn_ends_the_join() {
        let (_, code) = code();
        let mut join = Ask::join(&code, "M9", 0, 10_000).expect("join");
        let ok = wire::status_answer(Status::Ok);
        assert!(matches!(
            join.answered(Ok(&ok)),
            Step::Done(Err(HomeError::Transfer(_)))
        ));
    }

    fn home() -> [u8; 64] {
        code().1.public_key
    }

    fn entry(seq: u64) -> wire::Entry {
        wire::Entry {
            seq,
            kind: super::super::KIND_RECEIVED,
            at: 50,
            packed: alloc::vec![seq as u8; 120],
        }
    }

    /// Up to the request on a fresh ask.
    fn to_request(ask: &mut Ask) -> Step {
        assert!(matches!(ask.poll(0, Some(1)), Step::Connect { .. }));
        assert_eq!(ask.linked(Ok(())), Step::Identify);
        ask.identified(Ok(()))
    }

    /// The constant is the link core builds: a link of the default MTU.
    #[test]
    fn the_lora_page_fits_the_mdu_of_a_default_link() {
        let link = leviculum_core::link::Link::new_outgoing(
            leviculum_core::DestinationHash::new([0x42; 16]),
            &mut rand_core::OsRng,
        );
        assert_eq!(link.mdu(), LORA_LINK_MDU);
        assert_eq!(LORA_PAGE_BYTES, 412);
    }

    /// A pull asks its pages on one link, hands each out and holds none,
    /// and ends with the newest sequence as its cursor.
    #[test]
    fn a_pull_hands_out_each_page_and_asks_the_next_from_its_last() {
        let page_bytes = Some(LORA_PAGE_BYTES as u64);
        let verb = Verb::Pull {
            cursor: 3,
            page_bytes,
        };
        let mut ask = Ask::new(&home(), verb, 0, 60_000).expect("ask");
        let Step::Request { path, data } = to_request(&mut ask) else {
            panic!("no request");
        };
        assert_eq!(path, super::super::SYNC_PATH);
        assert_eq!(wire::read_sync_paged(&data), Ok((3, page_bytes)));
        let page = |entries: Vec<wire::Entry>, more| wire::Page {
            entries,
            more,
            first: 1,
            left: None,
        };
        let one = page(alloc::vec![entry(4), entry(5)], true);
        assert_eq!(
            ask.answered(Ok(&wire::sync_answer(&one))),
            Step::Page(one.clone())
        );
        let Step::Request { data, .. } = ask.poll(10, Some(1)) else {
            panic!("no second request");
        };
        assert_eq!(wire::read_sync_paged(&data), Ok((5, page_bytes)));
        let two = page(alloc::vec![entry(6)], false);
        assert_eq!(ask.answered(Ok(&wire::sync_answer(&two))), Step::Page(two));
        let pulled = Outcome::Pulled {
            cursor: 6,
            first: 1,
            entries: 3,
            pages: 2,
        };
        assert_eq!(ask.poll(20, Some(1)), Step::Done(Ok(pulled)));
        // A page that says more but brings nothing past the cursor ends
        // the pull instead of asking the same page forever.
        let verb = Verb::Pull {
            cursor: 9,
            page_bytes: None,
        };
        let mut ask = Ask::new(&home(), verb, 0, 60_000).expect("ask");
        to_request(&mut ask);
        let stuck = page(alloc::vec![entry(9)], true);
        assert_eq!(
            ask.answered(Ok(&wire::sync_answer(&stuck))),
            Step::Page(stuck)
        );
        assert!(matches!(
            ask.poll(1, Some(1)),
            Step::Done(Ok(Outcome::Pulled { cursor: 9, .. }))
        ));
    }

    /// A send whose answer is lost goes again on a fresh link, up to
    /// [`ATTEMPTS`]; a join is asked once, its token being single-use.
    #[test]
    fn a_lost_answer_asks_a_send_again_and_a_join_never() {
        let submission = wire::Submission {
            client_id: [1; 16],
            destination: [2; 16],
            title: Vec::new(),
            content: b"hi".to_vec(),
            fields: Vec::new(),
        };
        let mut ask = Ask::new(&home(), Verb::Send(submission), 0, 60_000).expect("ask");
        for attempt in 1..ATTEMPTS {
            to_request(&mut ask);
            assert_eq!(ask.attempts(), attempt);
            assert_eq!(ask.answered(Err(HomeError::Timeout)), Step::Close);
        }
        to_request(&mut ask);
        assert_eq!(
            ask.answered(Err(HomeError::Timeout)),
            Step::Done(Err(HomeError::Timeout))
        );
        let mut ask = Ask::new(
            &home(),
            Verb::Send(wire::Submission {
                client_id: [3; 16],
                destination: [2; 16],
                title: Vec::new(),
                content: Vec::new(),
                fields: Vec::new(),
            }),
            0,
            60_000,
        )
        .expect("ask");
        to_request(&mut ask);
        let id = [7u8; 32];
        assert_eq!(
            ask.answered(Ok(&wire::send_answer(&id))),
            Step::Done(Ok(Outcome::Sent(id)))
        );

        let (_, code) = code();
        let mut join = Ask::join(&code, "M9", 0, 60_000).expect("join");
        to_request(&mut join);
        assert_eq!(
            join.answered(Err(HomeError::Timeout)),
            Step::Done(Err(HomeError::Timeout))
        );
    }

    /// A mark and a group read their own answers.
    #[test]
    fn a_mark_and_a_group_read_their_answers() {
        let mark = wire::Mark {
            flag: wire::MarkFlag::Read,
            ids: alloc::vec![[5; 32]],
        };
        let mut ask = Ask::new(&home(), Verb::Mark(mark.clone()), 0, 60_000).expect("ask");
        let Step::Request { path, data } = to_request(&mut ask) else {
            panic!("no request");
        };
        assert_eq!(
            (path, wire::read_mark(&data)),
            (super::super::MARK_PATH, Ok(mark))
        );
        let ok = wire::status_answer(Status::Ok);
        assert_eq!(ask.answered(Ok(&ok)), Step::Done(Ok(Outcome::Marked)));

        let group = wire::GroupRequest {
            client_id: [4; 16],
            name: String::from("Kletterhalle"),
            members: alloc::vec![([6; 16], String::from("Ana"))],
        };
        let mut ask =
            Ask::new(&home(), Verb::Group(GroupVerb::Create(group)), 0, 60_000).expect("ask");
        to_request(&mut ask);
        let refused = wire::status_answer(Status::NotAdmitted);
        assert_eq!(
            ask.answered(Ok(&refused)),
            Step::Done(Err(HomeError::Refused(Status::NotAdmitted)))
        );
    }

    /// 1366: the group's other verbs ask their own path and read their own
    /// answer; a delete's `Ok` names the group it deleted, a state message
    /// is a send.
    #[test]
    fn the_group_verbs_ask_their_paths_and_read_their_answers() {
        let group = [9; 16];
        let add = wire::GroupAddRequest {
            client_id: [4; 16],
            group,
            members: alloc::vec![([6; 16], String::from("Ana"))],
        };
        let delete = wire::GroupDeleteRequest {
            client_id: [5; 16],
            group,
        };
        let state = wire::Submission {
            client_id: [6; 16],
            destination: group,
            title: Vec::new(),
            content: Vec::new(),
            fields: Vec::new(),
        };
        let cases = [
            (
                GroupVerb::Add(add.clone()),
                super::super::GROUP_ADD_PATH,
                wire::group_add_request(&add),
                wire::group_answer(&group),
                Outcome::Group(group),
            ),
            (
                GroupVerb::Delete(delete),
                super::super::GROUP_DELETE_PATH,
                wire::group_delete_request(&delete),
                wire::status_answer(Status::Ok),
                Outcome::Group(group),
            ),
            (
                GroupVerb::State(state.clone()),
                super::super::SEND_PATH,
                wire::send_request(&state),
                wire::send_answer(&[7; 32]),
                Outcome::Sent([7; 32]),
            ),
        ];
        for (verb, want_path, want_data, answer, outcome) in cases {
            let mut ask = Ask::new(&home(), Verb::Group(verb), 0, 60_000).expect("ask");
            let Step::Request { path, data } = to_request(&mut ask) else {
                panic!("no request");
            };
            assert_eq!((path, &data), (want_path, &want_data));
            assert_eq!(ask.answered(Ok(&answer)), Step::Done(Ok(outcome)));
        }
        // The control: a delete the home refuses is no group.
        let mut ask =
            Ask::new(&home(), Verb::Group(GroupVerb::Delete(delete)), 0, 60_000).expect("ask");
        to_request(&mut ask);
        assert_eq!(
            ask.answered(Ok(&wire::status_answer(Status::NotAdmitted))),
            Step::Done(Err(HomeError::Refused(Status::NotAdmitted)))
        );
    }
}
