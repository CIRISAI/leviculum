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

use leviculum_core::node::request::request_timeout_ms;
use leviculum_core::node::{NodeCore, NodeEvent};
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::{DestinationHash, Identity, LinkId, TickOutput};
use rand_core::CryptoRngCore;

pub mod rows;
pub mod store;

use super::ask::{self, Ask, HomeError, Outcome, Step};
use super::wire::{self, Status};
use crate::outbox::OutboxEvent;
use crate::ui::strings::{Arg, StrId, Text};

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
/// LoRa page (`ask::LORA_PAGE_BYTES`) unless the board's census of a
/// session's pull found less room than one LoRa page's pull takes. The
/// answer ceiling is not the session's: one entry larger than a page comes
/// alone and costs its own length, which is the message the store keeps
/// (`loquax_m9::census::SESSION_PEAK`'s note), so it is what the board
/// takes in for one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    /// The `/sync` page asked.
    pub page_bytes: u64,
    /// The longest answer kept; a longer one asks a smaller page.
    pub max_answer: usize,
}

impl Batch {
    /// The batch of a board whose census measured `peak` heap bytes for a
    /// session's pull in LoRa pages, held to `room`: the LoRa page when the
    /// peak fits, a page cut in proportion when it does not, never under
    /// [`MIN_PAGE_BYTES`]; answers of up to `max_answer` bytes.
    pub fn sized(peak: usize, room: usize, max_answer: usize) -> Self {
        let lora = ask::LORA_PAGE_BYTES as u64;
        let page_bytes = if peak <= room {
            lora
        } else {
            (lora.saturating_mul(room as u64) / peak as u64).clamp(MIN_PAGE_BYTES, lora)
        };
        Self {
            page_bytes,
            max_answer,
        }
    }

    /// Half the page, or `None` at [`MIN_PAGE_BYTES`].
    pub fn halved(page_bytes: u64) -> Option<u64> {
        (page_bytes > MIN_PAGE_BYTES).then(|| (page_bytes / 2).max(MIN_PAGE_BYTES))
    }
}

/// The link MTU a serial line negotiates: the HW_MTU every Leviculum and
/// Python serial or KISS interface declares
/// (`leviculum_serial_mux::SERIAL_HW_MTU`, `RNS/Interfaces/SerialInterface.py`).
/// A link that negotiated less crossed a radio (an RNode's 508) or no
/// hop said its MTU at all (the default 500).
pub const SERIAL_LINK_MTU: u32 = 564;

/// From this negotiated MTU on, every hop of the link is a wide medium:
/// UDP's 1064, AutoInterface's 1196, TCP's and a local client's 262144.
pub const WIDE_LINK_MTU: u32 = 1024;

/// A next hop that reports a bitrate under this is a radio (LoRa's
/// fastest setting is 62.5 kbit/s), whatever MTU the link negotiated.
pub const RADIO_BPS: u32 = 100_000;

/// The rate a serial line runs at when its interface reports none: the
/// M9's and every LNode's 115200 baud.
pub const SERIAL_LINE_BPS: u32 = 115_200;

/// A serial or wide link's page: what its line carries in this long, so
/// a page is a resource of many packets that still takes about a second.
pub const PAGE_MS: u64 = 1_000;

/// The kind of link a pull runs over, from what the link negotiated and
/// what the next hop reports (1439): a radio keeps one packet a page, a
/// serial line and a wide link get a page their rate carries in
/// [`PAGE_MS`], up to what the device takes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClass {
    /// LoRa somewhere on the way, or a link whose hops said no MTU.
    Radio,
    /// A serial line to a host's daemon, the M9's and an LNode's.
    Serial,
    /// TCP, UDP, a local client: no airtime cost anywhere.
    Wide,
}

impl LinkClass {
    /// The class of a link that negotiated `mtu`, its next hop reporting
    /// `next_hop_bps` when it reports one.
    pub fn of(mtu: u32, next_hop_bps: Option<u32>) -> Self {
        match (mtu, next_hop_bps) {
            (_, Some(bps)) if bps < RADIO_BPS => Self::Radio,
            (mtu, _) if mtu < SERIAL_LINK_MTU => Self::Radio,
            (mtu, _) if mtu < WIDE_LINK_MTU => Self::Serial,
            _ => Self::Wide,
        }
    }

    /// The `/sync` page a device in `batch` asks on this link, whose MDU
    /// is `mdu`, its next hop at `next_hop_bps` when it says: a radio's
    /// is one response packet and never more than the census allows
    /// (`batch.page_bytes`, the LoRa page or less); a serial line's and a
    /// wide link's is what the rate carries in [`PAGE_MS`], at least one
    /// packet, at most the answer the device takes in and the home's own
    /// page ([`super::SYNC_PAGE_BYTES`]). A wide link without a rate gets
    /// that most.
    pub fn page_bytes(self, mdu: usize, next_hop_bps: Option<u32>, batch: Batch) -> u64 {
        let packet = mdu.saturating_sub(ask::RESPONSE_OVERHEAD) as u64;
        let most = (batch.max_answer.min(super::SYNC_PAGE_BYTES) as u64).max(MIN_PAGE_BYTES);
        let carried = |bps: u32| u64::from(bps) / 10 * PAGE_MS / 1_000;
        let page = match (self, next_hop_bps) {
            (Self::Radio, _) => return packet.min(batch.page_bytes).max(MIN_PAGE_BYTES),
            (_, Some(bps)) => carried(bps),
            (Self::Serial, None) => carried(SERIAL_LINE_BPS),
            (Self::Wide, None) => most,
        };
        page.max(packet).min(most)
    }
}

/// The resource that answers the request in flight, watched for silence
/// (1461): its hash, the progress it last made and when.
#[derive(Debug, Clone, Copy)]
struct Answer {
    hash: [u8; 32],
    progress: f32,
    since_ms: u64,
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
    /// The batch a pull's page is fitted from once its first link is up
    /// (1439); `None` keeps the page the ask was made with.
    fit: Option<Batch>,
    /// The class and page the fit chose, once it did.
    fitted: Option<(LinkClass, u64)>,
    /// The resource answering the request in flight, once its
    /// advertisement was accepted (1461).
    answer: Option<Answer>,
    /// Whether a silent answer is cancelled and asked again (1461); off
    /// only for the control of the cell that measures the rule.
    watch_answers: bool,
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
            fit: None,
            fitted: None,
            answer: None,
            watch_answers: true,
        }
    }

    /// The executor without 1461's rule: a silent resource answer is left
    /// to the resource's own watchdog. Only the control of the cell that
    /// measures the rule wants this.
    #[doc(hidden)]
    pub fn without_answer_watch(self) -> Self {
        Self {
            watch_answers: false,
            ..self
        }
    }

    /// Perform `ask` in `batch`, a pull's page fitted to the link its first
    /// link comes up on (1439, [`LinkClass::page_bytes`]).
    pub fn fitted(ask: Ask, batch: Batch) -> Self {
        Self {
            fit: Some(batch),
            ..Self::new(ask, batch.max_answer)
        }
    }

    /// The link class and page the fit chose, once the first link was up.
    pub fn fit(&self) -> Option<(LinkClass, u64)> {
        self.fitted
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
        self.watch_answer(node, now_ms, out);
        let path = self.path(node);
        let step = self.ask.poll(now_ms, path);
        self.drive(node, device, now_ms, step, out)
    }

    /// 1461: the resource answering a repeatable request made no progress
    /// within the request bound of 1440 (six round trips of the link plus
    /// 11.25 s, `request_timeout_ms`): the line lost the answer whole
    /// while the link stays up. The resource's own watchdog asks for the
    /// missing parts with a growing wait, 16 times
    /// (`leviculum_core::resource::RESOURCE_MAX_RETRIES`, Python's
    /// `MAX_RETRIES`), which runs past a pull's budget; its asks keep
    /// running inside the bound and may still save the answer. Past it
    /// the board cancels the resource (an RCL, which the home expects)
    /// and the request fails at once as timed out, which
    /// [`Self::event`] makes a repeat on the same link ([`Ask::lost`],
    /// counted against [`ask::REPEATS`]). The ask's own budget ends it
    /// sooner when it runs out first.
    fn watch_answer<R: CryptoRngCore, C: Clock, S: Storage>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        if !self.watch_answers || self.request.is_none() || !self.ask.repeatable() {
            return;
        }
        let (Some(link_id), Some(answer)) = (self.link, self.answer.as_mut()) else {
            return;
        };
        let Some(link) = node.link(&link_id) else {
            self.answer = None;
            return;
        };
        let Some(progress) = link.incoming_resource_progress_of(&answer.hash) else {
            // Done or failed: the core says which with its own events.
            self.answer = None;
            return;
        };
        if progress > answer.progress {
            answer.progress = progress;
            answer.since_ms = now_ms;
            return;
        }
        let bound = request_timeout_ms(link.rtt_ms());
        if now_ms.saturating_sub(answer.since_ms) < bound {
            return;
        }
        let hash = answer.hash;
        self.answer = None;
        if let Ok(cancelled) = node.cancel_incoming_resource(&link_id, &hash) {
            out.merge(cancelled);
        }
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
                let link_id = *link_id;
                self.fit_link(node, &link_id);
                self.ask.linked(Ok(()))
            }
            NodeEvent::ResourceTransferStarted {
                link_id,
                resource_hash,
                is_sender: false,
            } if ours(link_id) && self.request.is_some() => {
                // The answer comes as a resource: watched for silence from
                // here on (1461).
                self.answer = Some(Answer {
                    hash: *resource_hash,
                    progress: 0.0,
                    since_ms: now_ms,
                });
                return Report::Idle;
            }
            NodeEvent::LinkClosed {
                link_id, reason, ..
            } if ours(link_id) => {
                self.link = None;
                self.answer = None;
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
            // A link gone stale with a request in flight carries no answer
            // any more: the line under it broke (a cable, a restarted
            // lnsd), and the request's own timeout is the whole ask's
            // budget. Asked again on a fresh link from the cursor, as a
            // closed link is (1415, the cell `m9_cell_link_breaks_mid_pull`:
            // without this the pull waited out its 120 s).
            NodeEvent::LinkStale { link_id } if ours(link_id) && self.request.is_some() => {
                self.request = None;
                self.ask
                    .answered(Err(HomeError::Transfer(String::from("link stale"))))
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
                // A link still up lost a frame of the answer, not the
                // home: the same request again on it (1440).
                if node.link(link_id).is_some_and(|link| link.is_active()) {
                    self.ask.lost()
                } else {
                    self.ask.answered(Err(HomeError::Timeout))
                }
            }
            _ => return Report::Idle,
        };
        self.drive(node, device, now_ms, step, out)
    }

    /// Fit a pull's page to the link that came up, once per ask: a link
    /// asked again after a break keeps the page, and with it any refit.
    fn fit_link<R: CryptoRngCore, C: Clock, S: Storage>(
        &mut self,
        node: &NodeCore<R, C, S>,
        link_id: &LinkId,
    ) {
        let (Some(batch), Some(link)) = (self.fit.take(), node.link(link_id)) else {
            return;
        };
        if self.ask.page_bytes().is_none() {
            return;
        }
        let bps = node.next_hop_interface_bitrate(self.destination.as_bytes());
        let class = LinkClass::of(link.negotiated_mtu(), bps);
        let page = class.page_bytes(link.mdu(), bps, batch);
        self.ask.fit_link(page);
        self.fitted = Some((class, page));
    }

    /// How long the request about to go waits for its answer (1440): for
    /// a request that may be asked again ([`Ask::lost`]), the bound
    /// Reticulum gives every request on a link, six round trips of the
    /// link plus 11.25 s of grace (`request_timeout_ms`, Python's
    /// `Link.request`), so a page whose answer the line lost is asked
    /// again within it rather than at the end of the ask's budget; on a
    /// serial link that is about 12 s, on a LoRa link with a 2 s round
    /// trip about 23 s. A response that comes as a resource is watched by
    /// the resource from its advertisement on, and by the same bound for
    /// silence ([`Self::watch_answer`], 1461). A join waits out its budget.
    /// Never past the budget.
    fn answer_wait_ms<R: CryptoRngCore, C: Clock, S: Storage>(
        &self,
        node: &NodeCore<R, C, S>,
        now_ms: u64,
    ) -> u64 {
        let left = self.ask.left_ms(now_ms);
        let rtt = self.link.as_ref().and_then(|link| node.link(link));
        match rtt {
            Some(link) if self.ask.repeatable() => request_timeout_ms(link.rtt_ms()).min(left),
            _ => left,
        }
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
                    let wait = self.answer_wait_ms(node, now_ms);
                    let sent = match self.link {
                        Some(link) => node
                            .send_request(&link, path, Some(&data), Some(wait))
                            .map_err(|error| format!("{error:?}")),
                        None => Err(String::from("no link")),
                    };
                    match sent {
                        Ok((request, sent)) => {
                            out.merge(sent);
                            self.request = Some(request);
                            self.answer = None;
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
        self.answer = None;
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

/// What the board's screen says of its home (1415): one word in the top
/// bar, the board's whole account of its client. The words are the
/// catalogue's ([`Word::text`]), so the phone and the desktop could say
/// the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    /// A code was taken in and its join runs, or waits for its next try.
    Joining,
    /// A pull took `taken` entries of the archive; `of` is how many there
    /// were in all when the home said so (`wire::Page::left`).
    Fetched { taken: u64, of: Option<u64> },
    /// The last ask got through and nothing is being pulled.
    Connected,
    /// The last ask found no path, no link or no answer.
    Unreachable,
    /// The home did not take the code: wrong, used or expired. Only a new
    /// code helps.
    CodeRefused,
    /// The home refused a device it once admitted.
    Refused,
}

impl Word {
    /// The words of it, in the catalogue.
    pub fn text(self) -> Text {
        match self {
            Self::Joining => Text::key(StrId::HomeJoining),
            Self::Fetched {
                taken,
                of: Some(of),
            } => Text::Key(
                StrId::HomeFetched,
                alloc::vec![Arg::Int(count(taken)), Arg::Int(count(of))],
            ),
            Self::Fetched { taken, of: None } => {
                Text::Key(StrId::HomeFetchedSome, alloc::vec![Arg::Int(count(taken))])
            }
            Self::Connected => Text::key(StrId::MailboxConnected),
            Self::Unreachable => Text::key(StrId::HomeUnreachable),
            Self::CodeRefused => Text::key(StrId::JoinRefused),
            Self::Refused => Text::key(StrId::MailboxNotConnected),
        }
    }

    /// Its one short word for a top bar (1564): a state that stands on
    /// its own ("Connected", "Fetching…"), where [`Word::text`] is the
    /// sentence a page with room says. No count: how far a pull came is
    /// the overview's to say, not the bar's.
    pub fn bar(self) -> Text {
        Text::key(match self {
            Self::Joining => StrId::M9BarConnecting,
            Self::Fetched { .. } => StrId::M9BarFetching,
            Self::Connected => StrId::M9BarConnected,
            Self::Unreachable => StrId::M9BarDisconnected,
            Self::CodeRefused | Self::Refused => StrId::M9BarRefused,
        })
    }

    /// The bar's word with the way to the home in it (1478): where the
    /// last ask got through and the board holds a path, which medium its
    /// first hop takes and how many hops it is, said as stations, the
    /// catalogue's word for them (1618: "Mailbox over radio · 2
    /// stations"); where the last ask found nothing and the board holds no
    /// path at all, that there is none ("No way to mailbox"). Every other
    /// state, and a state whose way is not known, says [`Word::bar`].
    pub fn bar_by(self, way: Option<Way>) -> Text {
        match (self, way) {
            (Self::Connected, Some(way)) => way.text(),
            (Self::Unreachable, None) => Text::key(StrId::M9BarNoWay),
            (word, _) => word.bar(),
        }
    }

    /// The shorter form of [`Word::bar_by`] where it has one, for a bar
    /// whose room does not hold the long one: the way without the
    /// mailbox's name ("Radio · 2 stations"), which the bar's right end only
    /// ever speaks of. None where the long form is already the short one.
    pub fn bar_by_short(self, way: Option<Way>) -> Option<Text> {
        match (self, way) {
            (Self::Connected, Some(way)) => Some(way.short()),
            _ => None,
        }
    }

    /// The word for a client in `session`: `admitted` or still joining,
    /// with `progress` of the pull in flight or just ended.
    pub fn of(session: &Session, admitted: bool, progress: Option<Progress>) -> Self {
        match (session.reach(), admitted, progress) {
            (Reach::Refused(_), false, _) => Self::CodeRefused,
            (Reach::Refused(_), true, _) => Self::Refused,
            (_, false, _) => match session.reach() {
                Reach::Unreachable => Self::Unreachable,
                _ => Self::Joining,
            },
            (_, true, Some(progress)) if progress.taken > 0 => Self::Fetched {
                taken: progress.taken,
                of: progress.of,
            },
            (Reach::Unreachable, true, _) => Self::Unreachable,
            (_, true, _) => Self::Connected,
        }
    }
}

/// The medium a path to the home leaves the board on (1478): what its
/// first hop's interface is, as the board names its interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    /// A radio interface: LoRa.
    Radio,
    /// A cable: the serial line to a host's daemon.
    Cable,
    /// The WLAN the board joined (1479): its AutoInterface, the LAN rung
    /// of the reachability ladder, a mailbox in the same network.
    Wifi,
}

/// The board's path to its home (1478): the medium of its first hop and
/// the hops the path table holds, 1 for a home the board hears directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Way {
    pub medium: Medium,
    pub hops: u8,
}

impl Way {
    /// "Mailbox over radio · 2 stations", "Mailbox over cable · 1 station".
    pub fn text(self) -> Text {
        let id = match (self.medium, self.hops == 1) {
            (Medium::Radio, true) => StrId::M9BarWayRadioOne,
            (Medium::Radio, false) => StrId::M9BarWayRadio,
            (Medium::Cable, true) => StrId::M9BarWayCableOne,
            (Medium::Cable, false) => StrId::M9BarWayCable,
            (Medium::Wifi, true) => StrId::M9BarWayWifiOne,
            (Medium::Wifi, false) => StrId::M9BarWayWifi,
        };
        Text::Key(id, alloc::vec![Arg::Int(i64::from(self.hops))])
    }

    /// "Radio · 2 stations", "Cable · 1 station": [`Way::text`] for a narrow bar.
    pub fn short(self) -> Text {
        let id = match (self.medium, self.hops == 1) {
            (Medium::Radio, true) => StrId::M9BarRadioOne,
            (Medium::Radio, false) => StrId::M9BarRadio,
            (Medium::Cable, true) => StrId::M9BarCableOne,
            (Medium::Cable, false) => StrId::M9BarCable,
            (Medium::Wifi, true) => StrId::M9BarWifiOne,
            (Medium::Wifi, false) => StrId::M9BarWifi,
        };
        Text::Key(id, alloc::vec![Arg::Int(i64::from(self.hops))])
    }
}

fn count(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// How far a pull came (1415): the entries taken, and how many there are
/// in all once a page said how many wait after it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub taken: u64,
    pub of: Option<u64>,
}

impl Progress {
    /// A page was taken.
    pub fn page(&mut self, page: &wire::Page) {
        self.taken = self.taken.saturating_add(page.entries.len() as u64);
        self.of = page.left.map(|left| self.taken.saturating_add(left));
    }
}

/// The client record a code makes (1415): the code as typed, pasted or
/// written over the USB control link, a new device identity, the name
/// the board joins under. Whitespace and case do not matter, and a code
/// that carries its rendezvous (`code@host:port,...`) is taken without
/// it: the board reaches its home over its own node.
pub fn admit(
    text: &str,
    device: &Identity,
    name: &str,
) -> Result<super::record::ClientRecord, AdmitError> {
    let (code, _) =
        super::code::HomeCode::parse_with_rendezvous(text.trim()).map_err(AdmitError::Code)?;
    super::record::ClientRecord::pending(device, &code, name).map_err(AdmitError::Record)
}

/// Why a code did not become a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitError {
    /// Not a home code: the prefix, a stray character, the checksum.
    Code(super::code::CodeError),
    /// The device identity has no private key.
    Record(super::record::RecordError),
}

/// How a board's record and its message store stood at boot (1476): the
/// cursor says how far the device pulled, the store how far what it
/// pulled is still there to show. They part when the store went (a board
/// whose messages lived in RAM and lost power, a store partition erased or
/// written over by another table), or when a power loss fell between a
/// page the store took and the record's write at the pull's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agreement {
    /// The record's cursor is the store's.
    Agrees,
    /// The store holds nothing while the record had pulled up to `was`:
    /// the store was lost, the cursor is reset and the board pulls again
    /// from the start.
    Lost { was: u64 },
    /// The store ends before the record's cursor `was`: the cursor goes
    /// back to the store's end and the board pulls the rest again.
    Behind { was: u64 },
    /// The store holds pages after the record's cursor `was`: a power loss
    /// before the record was written. The store's end is the cursor.
    Ahead { was: u64 },
}

/// Make `record`'s cursor agree with a store that ends at `stored` (the
/// newest sequence the store took, zero for an empty store or a board
/// without one), and say how they stood. The rule of 1476: a client
/// record with a cursor past zero and an empty store means the store was
/// lost, and the cursor resets, so the list fills again as soon as the
/// home is reachable instead of the cursor saying everything was fetched.
pub fn agree(record: &mut super::record::ClientRecord, stored: u64) -> Agreement {
    let was = record.cursor;
    record.cursor = stored;
    match (was, stored) {
        (was, stored) if was == stored => Agreement::Agrees,
        (was, 0) => Agreement::Lost { was },
        (was, stored) if stored < was => Agreement::Behind { was },
        (was, _) => Agreement::Ahead { was },
    }
}

impl core::fmt::Display for Agreement {
    /// The words of the `[HOME]` line: `store=empty cursor=reset was=5625`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Agrees => f.write_str("store=agrees"),
            Self::Lost { was } => write!(f, "store=empty cursor=reset was={was}"),
            Self::Behind { was } => write!(f, "store=behind cursor=back was={was}"),
            Self::Ahead { was } => write!(f, "store=ahead cursor=forward was={was}"),
        }
    }
}

/// The tags an ask is persisted under in the board's message store
/// (1493, [`store::Store::keep_ask`]). The payload is the request's wire
/// bytes, which the home's own readers parse back ([`ask_verb`]), so the
/// store needs no codec of its own.
pub const ASK_SEND: u8 = 1;
pub const ASK_MARK: u8 = 2;
pub const ASK_GROUP: u8 = 3;
pub const ASK_GROUP_ADD: u8 = 4;
pub const ASK_GROUP_DELETE: u8 = 5;

/// A verb as the store keeps it (1493): its tag and its request's wire
/// bytes. `None` for a join and a pull, which are never queued: the
/// record owes the join, and the session asks pulls on its own.
pub fn ask_bytes(verb: &ask::Verb) -> Option<(u8, alloc::vec::Vec<u8>)> {
    use super::ask::{GroupVerb, Verb};
    Some(match verb {
        Verb::Send(submission) => (ASK_SEND, wire::send_request(submission)),
        Verb::Mark(mark) => (ASK_MARK, wire::mark_request(mark)),
        Verb::Group(GroupVerb::Create(group)) => (ASK_GROUP, wire::group_request(group)),
        Verb::Group(GroupVerb::Add(add)) => (ASK_GROUP_ADD, wire::group_add_request(add)),
        Verb::Group(GroupVerb::Delete(delete)) => {
            (ASK_GROUP_DELETE, wire::group_delete_request(delete))
        }
        // A group state message is a send on the wire and comes back one.
        Verb::Group(GroupVerb::State(state)) => (ASK_SEND, wire::send_request(state)),
        Verb::Join { .. } | Verb::Pull { .. } => return None,
    })
}

/// The verb a kept ask replays as at boot (1493). `None` for a tag or
/// bytes this build cannot read: said in the log, never asked blindly.
pub fn ask_verb(tag: u8, request: &[u8]) -> Option<ask::Verb> {
    use super::ask::{GroupVerb, Verb};
    Some(match tag {
        ASK_SEND => Verb::Send(wire::read_send_request(request).ok()?),
        ASK_MARK => Verb::Mark(wire::read_mark(request).ok()?),
        ASK_GROUP => Verb::Group(GroupVerb::Create(wire::read_group_request(request).ok()?)),
        ASK_GROUP_ADD => Verb::Group(GroupVerb::Add(wire::read_group_add_request(request).ok()?)),
        ASK_GROUP_DELETE => Verb::Group(GroupVerb::Delete(
            wire::read_group_delete_request(request).ok()?,
        )),
        _ => return None,
    })
}

/// What one archive entry is for the board's model (1415), the board's
/// form of `client::take_in` without an engine: a message the home took
/// in for `address` (the home's LXMF address, which the board shows as
/// its own), one of the home's devices sent, a mark one of them set, a
/// name the home heard. `None` for the rest: their states and receipts
/// change bubbles the board does not keep yet, and a kind from a later
/// home the board cannot read. The board has not checked a received
/// message's signature: it has no sender's identity, so it says
/// `verified: false` and the model shows it so.
pub fn fold(entry: &wire::Entry, address: &[u8; 16]) -> Option<OutboxEvent> {
    use leviculum_lxmf::{DeliveryMethod, Message};
    match entry.kind {
        super::KIND_RECEIVED => {
            let message =
                Message::unpack(&entry.packed, None, None, DeliveryMethod::Direct).ok()?;
            (message.destination_hash == *address).then_some(OutboxEvent::Received {
                message_id: message.message_id,
                source: message.source_hash,
                title: message.title,
                body: message.content,
                fields: message.fields,
                verified: false,
                sent_at: message.timestamp as i64,
            })
        }
        super::KIND_SENT => {
            let message =
                Message::unpack(&entry.packed, None, None, DeliveryMethod::Direct).ok()?;
            Some(OutboxEvent::HomeSent {
                message_id: message.message_id,
                destination: message.destination_hash,
                sent_at: message.timestamp as i64,
                body: message.content,
                fields: message.fields,
            })
        }
        super::KIND_MARK => wire::read_mark(&entry.packed)
            .ok()
            .map(|mark| OutboxEvent::HomeMarked { mark }),
        super::KIND_NAME => wire::read_name_entry(&entry.packed)
            .ok()
            .map(|(destination, name)| OutboxEvent::Announced {
                destination,
                name: Some(name),
                path_response: false,
            }),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
