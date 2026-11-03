//! A client's ask of its home node without the waiting: the path ladder,
//! the link's budget and the join's steps, `no_std`, so the M9 runs the
//! same flow the desktop and the phone run (`handheld-messenger.md`,
//! "Networking: a client of a home, over LoRa"; 1359).
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

/// What the platform does next for a [`Join`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing: poll again later.
    Wait,
    /// Ask the network for a path to this destination.
    RequestPath([u8; 16]),
    /// Open a link to the home destination, checked against the home's
    /// signing key; then call [`Join::linked`].
    Connect {
        destination: [u8; 16],
        signing_key: [u8; 32],
    },
    /// Identify on the link with the device's own identity; then call
    /// [`Join::identified`].
    Identify,
    /// Send this request on the link; then call [`Join::answered`].
    Request { path: &'static str, data: Vec<u8> },
    /// The join is over: close the link if one was opened.
    Done(Result<(), HomeError>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Path,
    Linking,
    Identifying,
    Asking,
    Done(Result<(), HomeError>),
}

/// Redeeming an admission code as a device: a path to the home
/// destination, a link, the device identified on it, `/join` with the
/// code's token and the device's name, the status.
#[derive(Debug, Clone)]
pub struct Join {
    destination: [u8; 16],
    signing_key: [u8; 32],
    request: Vec<u8>,
    deadline_ms: u64,
    ladder: PathLadder,
    state: State,
}

impl Join {
    /// A join of `code` as the device called `name`, begun at `now_ms`
    /// with `budget_ms` to finish in.
    pub fn new(
        code: &HomeCode,
        name: &str,
        now_ms: u64,
        budget_ms: u64,
    ) -> Result<Self, HomeError> {
        let destination = super::destination_hash(&code.public_key).map_err(HomeError::NoLink)?;
        let mut signing_key = [0u8; 32];
        signing_key.copy_from_slice(&code.public_key[32..]);
        Ok(Self {
            destination,
            signing_key,
            request: wire::join_request(&code.token, name),
            deadline_ms: now_ms.saturating_add(budget_ms),
            ladder: PathLadder::new(),
            state: State::Path,
        })
    }

    /// The home destination the join links to.
    pub fn destination(&self) -> [u8; 16] {
        self.destination
    }

    /// How many path requests went.
    pub fn path_requests(&self) -> u32 {
        self.ladder.requests()
    }

    /// The next step at `now_ms`, with `path` the hop count of the path to
    /// [`Join::destination`] when the platform has one.
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
        if self.state != State::Path {
            return Step::Wait;
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
                Step::Request {
                    path: super::JOIN_PATH,
                    data: self.request.clone(),
                }
            }
            (State::Identifying, Err(detail)) => self.finish(Err(HomeError::NoLink(detail))),
            _ => self.out_of_turn(),
        }
    }

    /// The answer to `/join` came, or the request failed.
    pub fn answered(&mut self, result: Result<&[u8], HomeError>) -> Step {
        match (&self.state, result) {
            (State::Asking, Ok(answer)) => self.finish(join_outcome(answer)),
            (State::Asking, Err(error)) => self.finish(Err(error)),
            _ => self.out_of_turn(),
        }
    }

    fn finish(&mut self, result: Result<(), HomeError>) -> Step {
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
        let mut join = Join::new(&code, "M9", 1_000, 60_000).expect("join");
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
        assert_eq!(join.answered(Ok(&ok)), Step::Done(Ok(())));
        assert_eq!(join.poll(9_200, Some(3)), Step::Done(Ok(())));
        assert_eq!(join.path_requests(), 2);
    }

    /// A refusal is the home's status; no path in the budget is `NoPath`;
    /// a link over many hops gets the transport's time past the budget.
    #[test]
    fn a_refusal_no_path_and_a_long_link_end_as_they_should() {
        let (_, code) = code();
        let mut join = Join::new(&code, "M9", 0, 10_000).expect("join");
        join.poll(0, Some(1));
        join.linked(Ok(()));
        join.identified(Ok(()));
        let used = wire::status_answer(Status::BadToken);
        assert_eq!(
            join.answered(Ok(&used)),
            Step::Done(Err(HomeError::Refused(Status::BadToken)))
        );

        let mut join = Join::new(&code, "M9", 0, 10_000).expect("join");
        assert_eq!(
            join.poll(9_999, None),
            Step::RequestPath(join.destination())
        );
        assert_eq!(join.poll(10_000, None), Step::Done(Err(HomeError::NoPath)));

        let mut join = Join::new(&code, "M9", 0, 10_000).expect("join");
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
        let mut join = Join::new(&code, "M9", 0, 10_000).expect("join");
        let ok = wire::status_answer(Status::Ok);
        assert!(matches!(
            join.answered(Ok(&ok)),
            Step::Done(Err(HomeError::Transfer(_)))
        ));
    }
}
