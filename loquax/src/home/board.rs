//! The board's half of a client of its home (1414): the executor that
//! performs an [`Ask`]'s steps over `leviculum-core`'s own node, the
//! session that says whether the home is reachable and when to ask again,
//! and the pull batch the board's census sizes. `no_std`, so the M9 runs it
//! and the host tests the same code against a fake home on a second core
//! node.
//!
//! The host's executor is `client::run_ask` over `leviculum_std` and tokio.
//! A board has no runtime to wait on: its loop hands every packet to the
//! [`NodeCore`] and every event that comes back to [`Executor::event`],
//! and calls [`Executor::poll`] on each turn with its own clock. The
//! executor only ever adds to the caller's [`TickOutput`]; the board
//! dispatches it to its interfaces as it dispatches everything else.

use alloc::format;
use alloc::string::String;

use leviculum_core::node::{NodeCore, NodeEvent};
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::{DestinationHash, Identity, LinkId, TickOutput};
use rand_core::CryptoRngCore;

use super::ask::{self, Ask, HomeError, Outcome, Step};
use super::wire::{self, Status};
use crate::ui::strings::StrId;

/// The smallest page a pull shrinks to before it gives up on an answer
/// that does not fit: one short text message and its entry header.
pub const MIN_PAGE_BYTES: u64 = 64;

/// What an [`Executor`] has for the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// Nothing yet.
    Idle,
    /// A page of a pull: keep its entries; the ask moves on once polled.
    Page(wire::Page),
    /// The ask is over; reported once.
    Done(Result<Outcome, HomeError>),
}

/// The pull batch: the page a pull asks, and the largest answer the board
/// takes in before it asks a smaller page instead (1414). The page is the
/// LoRa page (`ask::LORA_PAGE_BYTES`) unless the census found less room;
/// the answer ceiling is what the board's heap holds for one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    /// The `/sync` page asked.
    pub page_bytes: u64,
    /// The longest answer kept; a longer one asks a smaller page.
    pub max_answer: usize,
}

impl Batch {
    /// The batch a board whose census measured `peak` heap bytes for one
    /// LoRa page's pull may run with `room` bytes to spare: the LoRa page
    /// when it fits, a page cut in proportion when it does not, never under
    /// [`MIN_PAGE_BYTES`]. An answer may take what the room allows.
    pub fn sized(peak: usize, room: usize) -> Self {
        let lora = ask::LORA_PAGE_BYTES as u64;
        let page_bytes = if peak == 0 || peak <= room {
            lora
        } else {
            (lora.saturating_mul(room as u64) / peak as u64).clamp(MIN_PAGE_BYTES, lora)
        };
        let per_byte = (peak / ask::LORA_PAGE_BYTES).max(1);
        Self {
            page_bytes,
            max_answer: (room / per_byte).max(MIN_PAGE_BYTES as usize),
        }
    }

    /// Half the page, or `None` at [`MIN_PAGE_BYTES`].
    pub fn halved(page_bytes: u64) -> Option<u64> {
        (page_bytes > MIN_PAGE_BYTES).then(|| (page_bytes / 2).max(MIN_PAGE_BYTES))
    }
}

/// One [`Ask`] performed over a core node.
#[derive(Debug)]
pub struct Executor {
    ask: Ask,
    destination: DestinationHash,
    link: Option<LinkId>,
    /// Set from the connect until the link is up or gone.
    linking: bool,
    /// The request whose answer is owed.
    request: Option<[u8; 16]>,
    max_answer: usize,
    reported: bool,
}

impl Executor {
    /// Perform `ask`, taking answers of at most `max_answer` bytes.
    pub fn new(ask: Ask, max_answer: usize) -> Self {
        Self {
            destination: DestinationHash::new(ask.destination()),
            ask,
            link: None,
            linking: false,
            request: None,
            max_answer,
            reported: false,
        }
    }

    /// The ask being performed.
    pub fn ask(&self) -> &Ask {
        &self.ask
    }

    /// The link the ask holds, while it holds one.
    pub fn link(&self) -> Option<LinkId> {
        self.link
    }

    /// One turn at `now_ms`: what the ask wants done, done.
    pub fn poll<R: CryptoRngCore, C: Clock, S: Storage>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        device: &Identity,
        now_ms: u64,
        out: &mut TickOutput,
    ) -> Report {
        let path = self.path(node);
        let step = self.ask.poll(now_ms, path);
        self.drive(node, device, now_ms, step, out)
    }

    /// An event the node raised; the ones about this ask move it on.
    pub fn event<R: CryptoRngCore, C: Clock, S: Storage>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        device: &Identity,
        event: &NodeEvent,
        now_ms: u64,
        out: &mut TickOutput,
    ) -> Report {
        let ours = |id: &LinkId| self.link.as_ref() == Some(id);
        let step = match event {
            NodeEvent::LinkEstablished { link_id, .. } if ours(link_id) && self.linking => {
                self.linking = false;
                self.ask.linked(Ok(()))
            }
            NodeEvent::LinkClosed {
                link_id, reason, ..
            } if ours(link_id) => {
                self.link = None;
                if self.linking {
                    self.linking = false;
                    self.ask.linked(Err(format!("link closed: {reason:?}")))
                } else if self.request.take().is_some() {
                    self.ask
                        .answered(Err(HomeError::Transfer(format!("link closed: {reason:?}"))))
                } else {
                    // Between two pages: the next request finds no link and
                    // the ask asks again on a fresh one.
                    return Report::Idle;
                }
            }
            NodeEvent::ResponseReceived {
                link_id,
                request_id,
                response_data,
                ..
            } if ours(link_id) && self.request == Some(*request_id) => {
                self.request = None;
                if response_data.len() > self.max_answer {
                    let smaller = self.ask.page_bytes().and_then(Batch::halved);
                    self.ask.refit(smaller.unwrap_or(u64::MAX))
                } else {
                    self.ask.answered(Ok(response_data))
                }
            }
            NodeEvent::RequestTimedOut {
                link_id,
                request_id,
            } if ours(link_id) && self.request == Some(*request_id) => {
                self.request = None;
                self.ask.answered(Err(HomeError::Timeout))
            }
            _ => return Report::Idle,
        };
        self.drive(node, device, now_ms, step, out)
    }

    fn path<R: CryptoRngCore, C: Clock, S: Storage>(&self, node: &NodeCore<R, C, S>) -> Option<u8> {
        node.has_path(&self.destination)
            .then(|| node.hops_to(&self.destination).unwrap_or(1))
    }

    /// Perform steps until one waits on the network or has something for
    /// the caller.
    fn drive<R: CryptoRngCore, C: Clock, S: Storage>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        device: &Identity,
        now_ms: u64,
        mut step: Step,
        out: &mut TickOutput,
    ) -> Report {
        loop {
            step = match step {
                Step::Wait => return Report::Idle,
                Step::RequestPath(_) => {
                    out.merge(node.request_path(&self.destination));
                    return Report::Idle;
                }
                Step::Connect { signing_key, .. } => {
                    match node.connect(self.destination, &signing_key) {
                        Ok((link, _, sent)) => {
                            out.merge(sent);
                            self.link = Some(link);
                            self.linking = true;
                            return Report::Idle;
                        }
                        Err(error) => self.ask.linked(Err(format!("{error:?}"))),
                    }
                }
                Step::Identify => match self.link {
                    Some(link) => match node.identify_link(&link, device) {
                        Ok(sent) => {
                            out.merge(sent);
                            self.ask.identified(Ok(()))
                        }
                        Err(error) => self.ask.identified(Err(format!("{error:?}"))),
                    },
                    None => self.ask.identified(Err(String::from("no link"))),
                },
                Step::Request { path, data } => {
                    let left = self.ask.left_ms(now_ms);
                    let sent = match self.link {
                        Some(link) => node
                            .send_request(&link, path, Some(&data), Some(left))
                            .map_err(|error| format!("{error:?}")),
                        None => Err(String::from("no link")),
                    };
                    match sent {
                        Ok((request, sent)) => {
                            out.merge(sent);
                            self.request = Some(request);
                            return Report::Idle;
                        }
                        Err(detail) => self.ask.answered(Err(HomeError::Transfer(detail))),
                    }
                }
                Step::Page(page) => return Report::Page(page),
                Step::Close => {
                    self.close(node, out);
                    let path = self.path(node);
                    self.ask.poll(now_ms, path)
                }
                Step::Done(result) => {
                    self.close(node, out);
                    if self.reported {
                        return Report::Idle;
                    }
                    self.reported = true;
                    return Report::Done(result);
                }
            };
        }
    }

    fn close<R: CryptoRngCore, C: Clock, S: Storage>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        out: &mut TickOutput,
    ) {
        self.request = None;
        self.linking = false;
        if let Some(link) = self.link.take() {
            out.merge(node.close_link(&link));
        }
    }
}

/// Whether the home answers, as the board shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Not asked yet since the board came up.
    Unknown,
    /// The last ask got through.
    Connected,
    /// The last ask found no path, no link or no answer.
    Unreachable,
    /// The home answered and refused: this device is not (or no longer)
    /// admitted, or the token was used.
    Refused(Status),
}

/// The first wait after an ask that did not get through.
pub const BACKOFF_FIRST_MS: u64 = 30_000;
/// The longest wait between two asks of a home that does not answer.
pub const BACKOFF_MAX_MS: u64 = 15 * 60_000;
/// How long after a pull that got through the next one goes, while no wake
/// says sooner.
pub const PULL_EVERY_MS: u64 = 5 * 60_000;

/// When the board asks its home next, and what it shows meanwhile: an ask
/// that does not get through doubles the wait from
/// [`BACKOFF_FIRST_MS`] up to [`BACKOFF_MAX_MS`], one that does resets it.
/// Inside one ask the path ladder already backs off
/// (`ask::PathLadder`); this is the ladder between asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    reach: Reach,
    next_ms: u64,
    backoff_ms: u64,
    failures: u32,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// A session that asks at once.
    pub const fn new() -> Self {
        Self {
            reach: Reach::Unknown,
            next_ms: 0,
            backoff_ms: BACKOFF_FIRST_MS,
            failures: 0,
        }
    }

    /// Whether an ask is due at `now_ms`.
    pub fn due(&self, now_ms: u64) -> bool {
        now_ms >= self.next_ms
    }

    /// When the next ask is due.
    pub fn next_ms(&self) -> u64 {
        self.next_ms
    }

    /// Asks in a row that did not get through.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// What the board shows about its home.
    pub fn reach(&self) -> Reach {
        self.reach
    }

    /// The sentence the board shows for it, when it shows one: the same
    /// words the phone uses for its mailbox.
    pub fn notice(&self) -> Option<StrId> {
        match self.reach {
            Reach::Unknown => None,
            Reach::Connected => Some(StrId::MailboxConnected),
            Reach::Unreachable | Reach::Refused(_) => Some(StrId::MailboxNotConnected),
        }
    }

    /// An ask ended at `now_ms` with `result`.
    pub fn finished(&mut self, now_ms: u64, result: &Result<Outcome, HomeError>) {
        match result {
            Ok(_) => {
                self.reach = Reach::Connected;
                self.failures = 0;
                self.backoff_ms = BACKOFF_FIRST_MS;
                self.next_ms = now_ms.saturating_add(PULL_EVERY_MS);
            }
            Err(HomeError::Refused(status)) => {
                self.reach = Reach::Refused(*status);
                self.failures = self.failures.saturating_add(1);
                self.next_ms = now_ms.saturating_add(BACKOFF_MAX_MS);
            }
            Err(_) => {
                self.reach = Reach::Unreachable;
                self.failures = self.failures.saturating_add(1);
                self.next_ms = now_ms.saturating_add(self.backoff_ms);
                self.backoff_ms = self.backoff_ms.saturating_mul(2).min(BACKOFF_MAX_MS);
            }
        }
    }

    /// Ask again now: a wake, a key press, a link that came up.
    pub fn hurry(&mut self, now_ms: u64) {
        self.next_ms = self.next_ms.min(now_ms);
    }
}

#[cfg(test)]
mod tests;
