//! How a client talks to its home node: a link to `loquax.home`, identified
//! with the device's own identity, one request, the answer, the link closed.
//! Over the driver's own link API (`leviculum_std::ReticulumNode`), next to
//! the engine rather than inside it: the engine runs under the core lock and
//! may not wait, and a pull is nothing but waiting.

use std::time::Duration;

use leviculum_core::{DestinationHash, Identity, LinkId};
use leviculum_std::ReticulumNode;

use super::ask::{Ask, Outcome, Step, Verb};
use super::wire::{self, Entry, Status};

/// How often a waiting path is looked for.
const PATH_POLL: Duration = Duration::from_millis(100);
/// How long a request a home built before it may not know waits for its
/// answer before the same link asks a request every home knows (1307 pass
/// five): a home answers `/group` from its own store, at once.
const KNOWN_WAIT: Duration = Duration::from_secs(15);

pub use super::ask::HomeError;

impl std::error::Error for HomeError {}

/// What one pull brought: every entry after the cursor, oldest first, and
/// the oldest sequence the home node still holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pulled {
    pub entries: Vec<Entry>,
    pub first: u64,
}

/// Redeem an admission token: link, identify as `device`, `/join`.
pub async fn join(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    token: &[u8; 16],
    name: &str,
    budget: Duration,
) -> Result<(), HomeError> {
    let verb = Verb::Join {
        token: *token,
        name: name.to_string(),
    };
    let ask = Ask::new(public_key, verb, 0, millis(budget))?;
    run_ask(node, device, ask, |_| {}).await.map(|_| ())
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Perform an [`Ask`]'s steps over `node` as `device`: the host's executor
/// of the client the M9 runs (1363). Its clock is the milliseconds since
/// the call, so `ask` is one begun at 0. Each page of a pull goes to
/// `page` before the next is asked.
pub async fn run_ask(
    node: &ReticulumNode,
    device: &Identity,
    ask: Ask,
    page: impl FnMut(wire::Page),
) -> Result<Outcome, HomeError> {
    run_ask_watching(node, device, ask, page, &mut |_| {}).await
}

/// [`run_ask`], telling `progress` how far a request that goes as a
/// resource has gone, 0.0 to 1.0, every [`WATCH_EVERY`] it moved (1662):
/// a photo or a file handed to the mailbox shows it in its bubble.
pub async fn run_ask_watching(
    node: &ReticulumNode,
    device: &Identity,
    mut ask: Ask,
    mut page: impl FnMut(wire::Page),
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<Outcome, HomeError> {
    let start = tokio::time::Instant::now();
    let now = || millis(start.elapsed());
    let destination = DestinationHash::new(ask.destination());
    let path = || {
        node.has_path(&destination)
            .then(|| node.hops_to(&destination).unwrap_or(1))
    };
    let mut link: Option<LinkId> = None;
    let mut step = ask.poll(now(), path());
    loop {
        step = match step {
            Step::Wait => {
                tokio::time::sleep(PATH_POLL).await;
                ask.poll(now(), path())
            }
            Step::RequestPath(_) => {
                if let Err(error) = node.request_path(&destination).await {
                    return Err(HomeError::NoLink(error.to_string()));
                }
                if ask.path_requests() > 1 {
                    tracing::info!(
                        "HOME looking for the mailbox: path request {}",
                        ask.path_requests()
                    );
                }
                tokio::time::sleep(PATH_POLL).await;
                ask.poll(now(), path())
            }
            Step::Connect { signing_key, .. } => {
                match node.connect_awaited(&destination, &signing_key).await {
                    Err(error) => ask.linked(Err(error.to_string())),
                    Ok((handle, established)) => {
                        link = Some(*handle.link_id());
                        let from = now();
                        let left = ask.left_ms(from);
                        let waited = tokio::time::timeout(Duration::from_millis(left), established);
                        match waited.await {
                            // The budget is spent: the ask says so itself.
                            Err(_) => ask.poll(from.saturating_add(left), None),
                            Ok(Err(error)) => ask.linked(Err(error.to_string())),
                            Ok(Ok(_)) => ask.linked(Ok(())),
                        }
                    }
                }
            }
            Step::Identify => match &link {
                Some(id) => ask.identified(
                    node.identify_link(id, device)
                        .await
                        .map_err(|error| error.to_string()),
                ),
                None => ask.identified(Err("no link".to_string())),
            },
            Step::Request { path, data } => {
                let deadline =
                    tokio::time::Instant::now() + Duration::from_millis(ask.left_ms(now()));
                let answer = match &link {
                    Some(id) => request_watching(node, id, path, &data, deadline, progress).await,
                    None => Err(HomeError::Transfer("no link".to_string())),
                };
                ask.answered(answer.as_deref().map_err(HomeError::clone))
            }
            Step::Page(got) => {
                page(got);
                ask.poll(now(), path())
            }
            Step::Close => {
                if let Some(id) = link.take() {
                    let _ = node.close_link(&id).await;
                }
                ask.poll(now(), path())
            }
            Step::Done(result) => {
                if let Some(id) = link.take() {
                    let _ = node.close_link(&id).await;
                }
                return result;
            }
        };
    }
}

/// Hand one message to the home node to send: link, identify as `device`,
/// `/send`. The message id the home node gave it, also when the same
/// `client_id` was handed in before. The [`Ask`] the M9 runs (1371), with
/// its one retry policy: a lost answer (the request or its answer failed,
/// or ran out of time) asks again with the same submission on a fresh
/// link, [`super::ask::ATTEMPTS`] times in all within `budget`, and the
/// home node answers a client id it has seen with the message it made
/// then, so a retry never sends twice.
pub async fn send(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    submission: &wire::Submission,
    budget: Duration,
) -> Result<[u8; 32], HomeError> {
    send_watching(node, public_key, device, submission, budget, &mut |_| {}).await
}

/// [`send`], telling `progress` how far a submission that goes as a
/// resource has gone, 0.0 to 1.0 (1662).
pub async fn send_watching(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    submission: &wire::Submission,
    budget: Duration,
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<[u8; 32], HomeError> {
    let ask = Ask::new(
        public_key,
        Verb::Send(submission.clone()),
        0,
        millis(budget),
    )?;
    match run_ask_watching(node, device, ask, |_| {}, progress).await? {
        Outcome::Sent(id) => Ok(id),
        _ => Err(HomeError::Malformed),
    }
}

/// Hand the home node one mark: link, identify as `device`, `/mark`,
/// asked again up to three times when the answer was lost, as
/// [`send`] does: the home node keeps a mark it has once. The
/// [`Ask`] the M9 runs.
pub async fn mark(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    mark: &wire::Mark,
    budget: Duration,
) -> Result<(), HomeError> {
    let ask = Ask::new(public_key, Verb::Mark(mark.clone()), 0, millis(budget))?;
    run_ask(node, device, ask, |_| {}).await.map(|_| ())
}

/// Hand the home node the reader's name for `peer`, `None` clearing it
/// (1139), asked again as a mark is: the home node keeps the newest.
pub async fn petname(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    peer: &[u8; 16],
    petname: Option<&str>,
    budget: Duration,
) -> Result<(), HomeError> {
    let data = wire::petname_entry(peer, petname);
    ask_retrying(node, public_key, device, super::PETNAME_PATH, &data, budget).await
}

/// Hand the home node `peer`'s public key from its contact code (1187),
/// asked again as a petname is: remembering it twice changes nothing.
pub async fn key(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    peer: &[u8; 16],
    peer_key: &[u8; 64],
    budget: Duration,
) -> Result<(), HomeError> {
    let data = wire::key_request(peer, peer_key);
    ask_retrying(node, public_key, device, super::KEY_PATH, &data, budget).await
}

/// Hand the home node the reader's word on `peer` (1146), asked again as a
/// petname is: the home node keeps the newest.
pub async fn peer_state(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    peer: &[u8; 16],
    state: &wire::PeerState,
    budget: Duration,
) -> Result<(), HomeError> {
    let data = wire::peer_state_entry(peer, state);
    ask_retrying(
        node,
        public_key,
        device,
        super::PEERSTATE_PATH,
        &data,
        budget,
    )
    .await
}

/// What the mailbox says of itself (1642, [`super::about`]): its name,
/// machine, version and devices. A home from before it drops the path
/// ([`HomeError::Unsupported`]).
pub async fn about(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    budget: Duration,
) -> Result<super::about::About, HomeError> {
    let bytes = answer_if_known(
        node,
        public_key,
        device,
        super::ABOUT_PATH,
        &super::about::about_request(),
        budget,
        KNOWN_WAIT,
    )
    .await?;
    super::about::read_about_answer(&bytes)
        .map_err(|_| HomeError::Malformed)?
        .map_err(HomeError::Refused)
}

/// The NomadNet pages the home heard and the identity's bookmarks that
/// changed after `since` (1579, [`super::pages`]): `/pages` page by page
/// until no more follow, each on a link of its own. A home from before it
/// drops the path ([`HomeError::Unsupported`]); the device keeps the list
/// it heard itself.
pub async fn pages(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    since: u64,
    budget: Duration,
) -> Result<super::pages::Delta, HomeError> {
    use super::pages;
    let deadline = tokio::time::Instant::now() + budget;
    let mut all = pages::Delta {
        stamp: since,
        more: false,
        rows: Vec::new(),
    };
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(HomeError::Timeout);
        }
        let data = pages::pages_request(all.stamp, None);
        let bytes = answer_if_known(
            node,
            public_key,
            device,
            super::PAGES_PATH,
            &data,
            left,
            KNOWN_WAIT,
        )
        .await?;
        let delta = pages::read_pages_answer(&bytes)
            .map_err(|_| HomeError::Malformed)?
            .map_err(HomeError::Refused)?;
        tracing::info!(
            "HOME pages since={} rows={} stamp={} more={} bytes={}",
            all.stamp,
            delta.rows.len(),
            delta.stamp,
            delta.more,
            bytes.len()
        );
        let moved = delta.stamp != all.stamp;
        all.stamp = delta.stamp;
        all.rows.extend(delta.rows);
        if !delta.more || !moved {
            return Ok(all);
        }
    }
}

/// Hand the home one bookmark set or cleared (1579), asked again as a
/// petname is: the home keeps the newer by stamp, so twice is once.
pub async fn bookmark(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    row: &super::pages::BookmarkRow,
    budget: Duration,
) -> Result<(), HomeError> {
    let data = super::pages::bookmark_request(row);
    let bytes = answer_if_known(
        node,
        public_key,
        device,
        super::BOOKMARK_PATH,
        &data,
        budget,
        KNOWN_WAIT,
    )
    .await?;
    match wire::read_status(&bytes).map_err(|_| HomeError::Malformed)? {
        Status::Ok => Ok(()),
        status => Err(HomeError::Refused(status)),
    }
}

/// One request whose answer is a status, on a link of its own, asked again
/// up to three times when the answer was lost: for requests the home node
/// keeps once however often they come.
async fn ask_retrying(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    path: &str,
    data: &[u8],
    budget: Duration,
) -> Result<(), HomeError> {
    let bytes = answer_retrying(node, public_key, device, path, data, budget).await?;
    match wire::read_status(&bytes).map_err(|_| HomeError::Malformed)? {
        Status::Ok => Ok(()),
        status => Err(HomeError::Refused(status)),
    }
}

/// Make a group on the home node (1307 pass two): `/group`, asked again
/// with the same request when the answer was lost; the home node answers
/// a client id it has seen with the group it made then. The group's
/// address.
pub async fn group(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    request: &wire::GroupRequest,
    budget: Duration,
) -> Result<[u8; 16], HomeError> {
    group_waiting(node, public_key, device, request, budget, KNOWN_WAIT).await
}

/// [`group`] whose every ask waits `wait` before the home is looked at.
async fn group_waiting(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    request: &wire::GroupRequest,
    budget: Duration,
    wait: Duration,
) -> Result<[u8; 16], HomeError> {
    let data = wire::group_request(request);
    let path = super::GROUP_PATH;
    let bytes = answer_if_known(node, public_key, device, path, &data, budget, wait).await?;
    wire::read_group_answer(&bytes)
        .map_err(|_| HomeError::Malformed)?
        .map_err(HomeError::Refused)
}

/// `/groupadd` (1307 G2): add members to the owner's group; the group's
/// address, or the status that refused it.
pub async fn group_add(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    request: &wire::GroupAddRequest,
    budget: Duration,
) -> Result<[u8; 16], HomeError> {
    let data = wire::group_add_request(request);
    let bytes = answer_retrying(
        node,
        public_key,
        device,
        super::GROUP_ADD_PATH,
        &data,
        budget,
    )
    .await?;
    wire::read_group_answer(&bytes)
        .map_err(|_| HomeError::Malformed)?
        .map_err(HomeError::Refused)
}

/// `/groupdelete` (1307 pass four): the owner's group is deleted for
/// everyone; asked again as a petname is, since a second delete is `Ok`.
pub async fn group_delete(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    request: &wire::GroupDeleteRequest,
    budget: Duration,
) -> Result<(), HomeError> {
    let data = wire::group_delete_request(request);
    ask_retrying(
        node,
        public_key,
        device,
        super::GROUP_DELETE_PATH,
        &data,
        budget,
    )
    .await
}

/// One request on a link of its own, asked again up to three times when
/// the answer was lost: its answer's bytes.
async fn answer_retrying(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    path: &str,
    data: &[u8],
    budget: Duration,
) -> Result<Vec<u8>, HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let answer = match open(node, public_key, device, deadline).await {
            Ok(link) => {
                let answer = request(node, &link, path, data, deadline).await;
                let _ = node.close_link(&link).await;
                answer
            }
            Err(error) => Err(error),
        };
        match answer {
            Ok(bytes) => return Ok(bytes),
            Err(HomeError::Transfer(_) | HomeError::Timeout)
                if attempt < 3 && tokio::time::Instant::now() < deadline => {}
            Err(error) => return Err(error),
        }
    }
}

/// A request a home built before `path` drops without a word (its core
/// has no handler for it): asked as [`answer_retrying`] does, but each ask
/// waits `wait` ([`KNOWN_WAIT`]), and a silence is followed on the same link by
/// `/sync` past the newest entry, which every home answers with an empty
/// page. Two silences, each beside an answered `/sync`, are a home that
/// does not know `path` ([`HomeError::Unsupported`]); one is a lost
/// packet, and the same request goes again. `path` must be one the home
/// answers once however often it comes.
async fn answer_if_known(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    path: &'static str,
    data: &[u8],
    budget: Duration,
    wait: Duration,
) -> Result<Vec<u8>, HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut attempt = 0;
    let mut silences = 0;
    loop {
        attempt += 1;
        let answer = match open(node, public_key, device, deadline).await {
            Ok(link) => {
                let asked = deadline.min(tokio::time::Instant::now() + wait);
                let answer = match request(node, &link, path, data, asked).await {
                    Err(HomeError::Timeout) if tokio::time::Instant::now() < deadline => {
                        let probe = wire::sync_request(u64::MAX);
                        match request(node, &link, super::SYNC_PATH, &probe, deadline).await {
                            Ok(_) => {
                                silences += 1;
                                tracing::info!(
                                    "HOME {path} unanswered, /sync answered ({silences})"
                                );
                                Err(HomeError::Unsupported(path))
                            }
                            Err(error) => Err(error),
                        }
                    }
                    answer => answer,
                };
                let _ = node.close_link(&link).await;
                answer
            }
            Err(error) => Err(error),
        };
        match answer {
            Ok(bytes) => return Ok(bytes),
            Err(HomeError::Unsupported(_)) if silences < 2 => {}
            Err(HomeError::Transfer(_) | HomeError::Timeout)
                if attempt < 3 && tokio::time::Instant::now() < deadline => {}
            Err(error) => return Err(error),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(HomeError::Timeout);
        }
    }
}

/// A fresh id for one submission. The same id handed in twice is one
/// message, so a retry after a lost answer reuses it.
pub fn client_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut id);
    id
}

/// Pull everything after `cursor`, page by page over one link: the
/// [`Ask`] the M9 runs, in the home's own pages, asked again on a fresh
/// link when an answer was lost.
pub async fn pull(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    cursor: u64,
    budget: Duration,
) -> Result<Pulled, HomeError> {
    let verb = Verb::Pull {
        cursor,
        page_bytes: None,
    };
    let ask = Ask::new(public_key, verb, 0, millis(budget))?;
    let mut pulled = Pulled::default();
    run_ask(node, device, ask, |page| {
        pulled.first = page.first;
        pulled.entries.extend(page.entries);
    })
    .await?;
    Ok(pulled)
}

/// What a move's pages brought (1281): the identity's private key, its
/// ratchets, the old node's known identities, the mailbox's name, and the
/// archive entries after the cursor asked, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Taken {
    pub private_key: Vec<u8>,
    pub ratchets: Vec<u8>,
    pub known: Vec<u8>,
    pub name: String,
    pub entries: Vec<wire::Moving>,
}

/// Take a move's pages after `cursor` over one link (1281): `/move` with
/// the move's token until no more follow.
pub async fn take_move(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    token: &[u8; 16],
    mut cursor: u64,
    budget: Duration,
) -> Result<Taken, HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let link = open(node, public_key, device, deadline).await?;
    let mut taken = Taken::default();
    let result = loop {
        let answer = match request(
            node,
            &link,
            super::MOVE_PATH,
            &wire::move_request(token, cursor),
            deadline,
        )
        .await
        {
            Ok(answer) => answer,
            Err(error) => break Err(error),
        };
        let page = match wire::read_move_answer(&answer) {
            Ok(Ok(page)) => page,
            Ok(Err(status)) => break Err(HomeError::Refused(status)),
            Err(_) => break Err(HomeError::Malformed),
        };
        taken.private_key = page.private_key;
        taken.ratchets = page.ratchets;
        taken.known = page.known;
        taken.name = page.name;
        let last = page.entries.last().map(|entry| entry.seq);
        taken.entries.extend(page.entries);
        match (page.more, last) {
            (true, Some(last)) if last > cursor => cursor = last,
            _ => break Ok(taken),
        }
    };
    let _ = node.close_link(&link).await;
    result
}

/// Tell the old mailbox the move took (1281): `/moved` with the token and
/// the newest sequence taken. `Refused(Behind)` when it holds more.
pub async fn confirm_move(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    token: &[u8; 16],
    cursor: u64,
    budget: Duration,
) -> Result<(), HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let link = open(node, public_key, device, deadline).await?;
    let answer = request(
        node,
        &link,
        super::MOVED_PATH,
        &wire::move_request(token, cursor),
        deadline,
    )
    .await;
    let _ = node.close_link(&link).await;
    match wire::read_status(&answer?).map_err(|_| HomeError::Malformed)? {
        Status::Ok => Ok(()),
        status => Err(HomeError::Refused(status)),
    }
}

/// Whether a mailbox answers under `public_key` right now (1281, the
/// restore's guard): a link to its home destination comes up within
/// `budget`. Any answer counts, a refusal too; only silence is "no".
pub async fn answers(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    budget: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    match open(node, public_key, device, deadline).await {
        Ok(link) => {
            let _ = node.close_link(&link).await;
            true
        }
        Err(_) => false,
    }
}

/// A link to the home destination of `public_key`, identified as `device`.
pub(crate) async fn open(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    deadline: tokio::time::Instant,
) -> Result<LinkId, HomeError> {
    let destination =
        DestinationHash::new(super::destination_hash(public_key).map_err(HomeError::NoLink)?);
    let start = tokio::time::Instant::now();
    let mut ladder = super::ask::PathLadder::new();
    while !node.has_path(&destination) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            tracing::info!(
                "HOME no path to the mailbox after {} path request(s); the next \
                 attempt asks again",
                ladder.requests()
            );
            return Err(HomeError::NoPath);
        }
        let since = u64::try_from(now.duration_since(start).as_millis()).unwrap_or(u64::MAX);
        if ladder.due(since) {
            if ladder.requests() > 1 {
                tracing::info!(
                    "HOME looking for the mailbox: path request {}, the next in {} s",
                    ladder.requests(),
                    ladder.retry_ms() / 1000
                );
            }
            node.request_path(&destination)
                .await
                .map_err(|e| HomeError::NoLink(e.to_string()))?;
        }
        tokio::time::sleep(PATH_POLL).await;
    }
    // Rule 3 (1196): a link over a long path may take the transport's own
    // establishment time, however short the caller's budget was set on a
    // TCP round trip's reckoning (`ask::establishment_ms`).
    let hops = node.hops_to(&destination).unwrap_or(1);
    let deadline = deadline.max(
        tokio::time::Instant::now() + Duration::from_millis(super::ask::establishment_ms(hops)),
    );
    let mut signing_key = [0u8; 32];
    signing_key.copy_from_slice(&public_key[32..]);
    let (handle, established) = node
        .connect_awaited(&destination, &signing_key)
        .await
        .map_err(|e| HomeError::NoLink(e.to_string()))?;
    let link = *handle.link_id();
    match tokio::time::timeout_at(deadline, established).await {
        Err(_) => {
            let _ = node.close_link(&link).await;
            return Err(HomeError::Timeout);
        }
        Ok(Err(error)) => return Err(HomeError::NoLink(error.to_string())),
        Ok(Ok(_)) => {}
    }
    if let Err(error) = node.identify_link(&link, device).await {
        let _ = node.close_link(&link).await;
        return Err(HomeError::NoLink(error.to_string()));
    }
    Ok(link)
}

/// Ask the propagation node `pn` what became of `ids` (1409,
/// `docs/src/concepts/loquax-propagation-receipts.md` section B): a link
/// of its own to the node's `lxmf.propagation` destination, **never
/// identified**, so the node cannot tie the asker to the ids; one
/// `/lev/status` request with the encoded ids; the answer's bytes; the
/// link closed. The node's key comes from its announce
/// (`ReticulumNode::get_identity`); a node never heard has none and is not
/// asked. Shared by the home's sync round and a device that is its own
/// node.
pub async fn ask_pn_status(
    node: &ReticulumNode,
    pn: [u8; 16],
    ids: &[[u8; 32]],
    budget: Duration,
) -> Result<Vec<u8>, HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let destination = DestinationHash::new(pn);
    let mut ladder = super::ask::PathLadder::new();
    let start = tokio::time::Instant::now();
    while !node.has_path(&destination) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(HomeError::NoPath);
        }
        let since = u64::try_from(now.duration_since(start).as_millis()).unwrap_or(u64::MAX);
        if ladder.due(since) {
            node.request_path(&destination)
                .await
                .map_err(|e| HomeError::NoLink(e.to_string()))?;
        }
        tokio::time::sleep(PATH_POLL).await;
    }
    let identity = node
        .get_identity(&destination)
        .ok_or_else(|| HomeError::NoLink("the node's key is not known".to_string()))?;
    let mut signing_key = [0u8; 32];
    signing_key.copy_from_slice(&identity.public_key_bytes()[32..]);
    let (handle, established) = node
        .connect_awaited(&destination, &signing_key)
        .await
        .map_err(|e| HomeError::NoLink(e.to_string()))?;
    let link = *handle.link_id();
    match tokio::time::timeout_at(deadline, established).await {
        Err(_) => {
            let _ = node.close_link(&link).await;
            return Err(HomeError::Timeout);
        }
        Ok(Err(error)) => return Err(HomeError::NoLink(error.to_string())),
        Ok(Ok(_)) => {}
    }
    let data = leviculum_lxmf::lev_status::encode_request(ids);
    let answer = request(
        node,
        &link,
        leviculum_lxmf::lev_status::LEV_STATUS_PATH,
        &data,
        deadline,
    )
    .await;
    let _ = node.close_link(&link).await;
    answer
}

/// [`ask_pn_status`], read: the statuses, or `None` when the link failed,
/// the request went unanswered or the node refused, said in the log. What
/// a device that is its own node folds as
/// [`crate::ui::UiEvent::PnAnswered`].
pub async fn pn_answer(
    node: &ReticulumNode,
    pn: [u8; 16],
    ids: &[[u8; 32]],
    budget: Duration,
) -> Option<Vec<([u8; 32], leviculum_lxmf::lev_status::MessageStatus)>> {
    match ask_pn_status(node, pn, ids, budget).await {
        Ok(bytes) => {
            let answer = crate::pn_status::read_answer(&bytes);
            if answer.is_none() {
                tracing::info!(
                    "PN_STATUS refused node={} bytes={}",
                    crate::address::to_hex(&pn),
                    bytes.len()
                );
            }
            answer
        }
        Err(error) => {
            tracing::info!(
                "PN_STATUS unanswered node={}: {error}",
                crate::address::to_hex(&pn)
            );
            None
        }
    }
}

/// One request on `link` and its answer's bytes.
/// How often a request under way is asked how far its resource went
/// (1662): the bubble takes an update at most this often.
pub const WATCH_EVERY: Duration = Duration::from_millis(250);

/// [`request`], telling `progress` how far the request's resource has
/// gone each [`WATCH_EVERY`] it moved; a request that goes as one packet
/// tells nothing.
async fn request_watching(
    node: &ReticulumNode,
    link: &LinkId,
    path: &str,
    data: &[u8],
    deadline: tokio::time::Instant,
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<Vec<u8>, HomeError> {
    let asked = request(node, link, path, data, deadline);
    tokio::pin!(asked);
    let mut tick = tokio::time::interval(WATCH_EVERY);
    let mut said = None;
    loop {
        tokio::select! {
            answer = &mut asked => return answer,
            _ = tick.tick() => {
                if let Some(share) = node.link_outgoing_resource_progress(link) {
                    if said != Some(share.to_bits()) {
                        said = Some(share.to_bits());
                        progress(share);
                    }
                }
            }
        }
    }
}

pub(crate) async fn request(
    node: &ReticulumNode,
    link: &LinkId,
    path: &str,
    data: &[u8],
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, HomeError> {
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    let timeout_ms = u64::try_from(left.as_millis()).unwrap_or(u64::MAX);
    let (_, response) = node
        .send_request_awaited(link, path, Some(data), Some(timeout_ms))
        .await
        .map_err(|e| HomeError::Transfer(e.to_string()))?;
    match tokio::time::timeout_at(deadline, response).await {
        // The protocol's own timeout is the same silence as ours.
        Err(_) | Ok(Err(leviculum_std::driver::CompletionError::RequestTimedOut)) => {
            Err(HomeError::Timeout)
        }
        Ok(Err(error)) => Err(HomeError::Transfer(error.to_string())),
        Ok(Ok(info)) => Ok(info.response_data),
    }
}

/// The file under the loquax home that makes this device a client: the home
/// identity's public key in hex, written by `loquax home join`.
pub const CLIENT_FILE: &str = "home-client";
/// The device's own RNS identity, which the home node admitted.
pub const DEVICE_IDENTITY_FILE: &str = "device-identity";
/// The last archive sequence this client took in.
pub const CURSOR_FILE: &str = "home-cursor";

/// The home identity's public key, when this device is a client.
pub fn load_client(home: &std::path::Path) -> Result<Option<[u8; 64]>, String> {
    let path = home.join(CLIENT_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let text = text.trim();
    let mut key = [0u8; 64];
    let parsed = text.len() == 128
        && key.iter_mut().enumerate().all(|(i, byte)| {
            u8::from_str_radix(&text[2 * i..2 * i + 2], 16)
                .map(|value| *byte = value)
                .is_ok()
        });
    match parsed {
        true => Ok(Some(key)),
        false => Err(format!(
            "{}: not a home identity's public key (128 hex characters)",
            path.display()
        )),
    }
}

/// Make this device a client of the home identity whose key this is.
pub fn save_client(home: &std::path::Path, public_key: &[u8; 64]) -> Result<(), String> {
    let text: String = public_key.iter().map(|b| format!("{b:02x}")).collect();
    write_atomically(&home.join(CLIENT_FILE), &format!("{text}\n"))
}

/// The cursor this client pulls from: 0 before the first pull.
pub fn load_cursor(home: &std::path::Path) -> u64 {
    std::fs::read_to_string(home.join(CURSOR_FILE))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// Whether the next pull is the home's history, not live mail (1429): no
/// pull has settled here yet, so it starts at cursor 0 and brings the whole
/// archive, on a device that just joined or a phone the mailbox moved to.
/// The screen marks none of it read by arriving
/// ([`crate::ui::UiModel::history`]).
pub fn history_owed(home: &std::path::Path) -> bool {
    load_cursor(home) == 0
}

/// Move the cursor: only once what was pulled up to it is kept where the
/// caller keeps it ([`Pull::cursor`]).
pub fn save_cursor(home: &std::path::Path, cursor: u64) -> Result<(), String> {
    write_atomically(&home.join(CURSOR_FILE), &format!("{cursor}\n"))
}

/// Write `text` to `path` through a temporary file beside it, so a reader
/// sees the old content or the new, never half of it.
pub fn write_atomically(path: &std::path::Path, text: &str) -> Result<(), String> {
    let temporary = path.with_extension("new");
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// One pull into the engine, and what came of it.
#[derive(Debug, Default)]
pub struct Pull {
    /// How many entries came.
    pub entries: usize,
    /// The engine's report of each received message, in order.
    pub received: Vec<crate::outbox::OutboxEvent>,
    /// Each entry of a message sent through the home node: its id and its
    /// archive kind.
    pub sent: Vec<([u8; 32], u8)>,
    /// Each name the home node heard a peer announce, as archived.
    pub names: Vec<([u8; 16], String)>,
    /// Each imported history entry, as archived (1150): the frontend's
    /// store takes them before the cursor passes them.
    pub imported: Vec<Vec<u8>>,
    /// The cursor to save once what came is kept: `None` when nothing came,
    /// or when the engine did not get through it in time, so the next pull
    /// asks for the same entries again.
    pub cursor: Option<u64>,
}

impl Pull {
    /// How many received messages came: what a status line calls new. An
    /// entry of the user's own sent message, a state change, a mark and a
    /// name are entries, not messages (1136).
    pub fn messages(&self) -> usize {
        self.received.len()
    }

    /// How many state changes of messages sent through the home node came.
    pub fn states(&self) -> usize {
        self.sent
            .iter()
            .filter(|(_, kind)| *kind != super::KIND_SENT)
            .count()
    }
}

/// How long a pull waits for the engine to take the entries it was handed:
/// the engine takes commands every 200 ms.
const SETTLE: Duration = Duration::from_secs(5);

/// One pull into the engine: everything after the saved cursor, handed to
/// `outbox` in sequence order, a received message as
/// [`crate::outbox::Command::Ingest`] and a state change of a message sent
/// through the home node as [`crate::outbox::Command::HomeState`], then a
/// [`crate::outbox::Command::Fence`] behind them.
///
/// The cursor is not moved here. Once the fence is answered, every event the
/// entries caused was handed out before it, and [`Pull::cursor`] names the
/// cursor to save; the caller saves it after it kept those events (the
/// screen's store, the fetch's seen file), so a crash in between pulls the
/// same entries again rather than losing them.
pub async fn pull_into(
    outbox: &mut impl crate::outbox::Outbox,
    node: &ReticulumNode,
    home: &std::path::Path,
    public_key: &[u8; 64],
    device: &Identity,
    budget: Duration,
) -> Result<Pull, HomeError> {
    let cursor = load_cursor(home);
    let pulled = pull(node, public_key, device, cursor, budget).await?;
    take_in(outbox, cursor, pulled).await
}

/// The engine's half of [`pull_into`]: hand on what came after `cursor`,
/// fence it, and wait for the fence. Moves no cursor.
async fn take_in(
    outbox: &mut impl crate::outbox::Outbox,
    cursor: u64,
    pulled: Pulled,
) -> Result<Pull, HomeError> {
    use crate::outbox::{Command, OutboxEvent};
    if pulled.first > cursor.saturating_add(1) && cursor > 0 {
        tracing::warn!(
            "HOME pull: the mailbox no longer holds entries {}..{} (its archive cap)",
            cursor + 1,
            pulled.first - 1
        );
    }
    let gone = |gone: crate::outbox::OutboxGone| HomeError::Transfer(gone.to_string());
    let mut pull = Pull {
        entries: pulled.entries.len(),
        ..Pull::default()
    };
    let mut last = cursor;
    for entry in &pulled.entries {
        last = last.max(entry.seq);
        if entry.kind == super::KIND_RECEIVED {
            outbox
                .submit(Command::Ingest {
                    packed: entry.packed.clone(),
                })
                .map_err(gone)?;
            continue;
        }
        if entry.kind == super::KIND_MARK {
            match wire::read_mark(&entry.packed) {
                Ok(mark) => outbox.submit(Command::HomeMark { mark }).map_err(gone)?,
                Err(_) => tracing::warn!("HOME pull: entry {} is no mark", entry.seq),
            }
            continue;
        }
        if entry.kind == super::KIND_PETNAME {
            match wire::read_petname_entry(&entry.packed) {
                Ok((destination, petname)) => outbox
                    .submit(Command::HomePetname {
                        destination,
                        petname,
                        at: entry.at,
                    })
                    .map_err(gone)?,
                Err(_) => tracing::warn!("HOME pull: entry {} is no petname", entry.seq),
            }
            continue;
        }
        if entry.kind == super::KIND_PEERSTATE {
            match wire::read_peer_state_entry(&entry.packed) {
                Ok((destination, state)) => outbox
                    .submit(Command::HomePeerState {
                        destination,
                        state,
                        at: entry.at,
                    })
                    .map_err(gone)?,
                Err(_) => tracing::warn!("HOME pull: entry {} is no peer state", entry.seq),
            }
            continue;
        }
        if entry.kind == super::KIND_NAME {
            match wire::read_name_entry(&entry.packed) {
                Ok((destination, name)) => {
                    pull.names.push((destination, name.clone()));
                    outbox
                        .submit(Command::HomeName { destination, name })
                        .map_err(gone)?
                }
                Err(_) => tracing::warn!("HOME pull: entry {} is no name", entry.seq),
            }
            continue;
        }
        if entry.kind == super::KIND_HUB {
            // A hub the home heard (1599): every device lists it.
            // Its rooms when the home asked it (1643): the sheet lists
            // them before this device links to it.
            match wire::read_hub_entry_rooms(&entry.packed) {
                Ok((hub, rooms)) => {
                    let hash = hub.hash;
                    outbox.submit(Command::HomeHub { hub }).map_err(gone)?;
                    if let Some(rooms) = rooms {
                        outbox
                            .submit(Command::HomeHubRooms { hub: hash, rooms })
                            .map_err(gone)?;
                    }
                }
                Err(_) => tracing::warn!("HOME pull: entry {} is no hub", entry.seq),
            }
            continue;
        }
        if entry.kind == super::KIND_IMPORTED {
            pull.imported.push(entry.packed.clone());
            continue;
        }
        if entry.kind == super::KIND_PN_STATUS {
            // What the propagation node said of messages the home sent
            // (1409): every device folds it as one that asked itself.
            match wire::read_pn_status_entry(&entry.packed) {
                Ok(answers) => outbox
                    .submit(Command::HomePnStatus { answers })
                    .map_err(gone)?,
                Err(_) => tracing::warn!("HOME pull: entry {} is no pn status", entry.seq),
            }
            continue;
        }
        if entry.kind == super::KIND_GROUP {
            // The home's own record of a group it hosts; the owner's copy
            // came as a received message.
            continue;
        }
        let id = match entry.kind {
            super::KIND_SENT => leviculum_lxmf::Message::unpack(
                &entry.packed,
                None,
                None,
                leviculum_lxmf::DeliveryMethod::Direct,
            )
            .ok()
            .map(|message| message.message_id),
            _ => <[u8; 32]>::try_from(entry.packed.as_slice()).ok(),
        };
        let Some(id) = id else {
            tracing::warn!(
                "HOME pull: entry {} of kind {} names no message",
                entry.seq,
                entry.kind
            );
            continue;
        };
        if super::sent_word(entry.kind).is_none() {
            // A kind from a later home node: nothing this client can show.
            continue;
        }
        pull.sent.push((id, entry.kind));
        if entry.kind == super::KIND_SENT {
            // The bubble itself, on a device that does not have it yet.
            outbox
                .submit(Command::HomeSent {
                    packed: entry.packed.clone(),
                })
                .map_err(gone)?;
        } else if entry.kind == super::KIND_STALLED {
            outbox
                .submit(Command::HomeStalled { message_id: id })
                .map_err(gone)?;
        } else if let Some(state) = super::shown_state(entry.kind) {
            outbox
                .submit(Command::HomeState {
                    message_id: id,
                    state,
                })
                .map_err(gone)?;
        }
    }
    if last == cursor {
        tracing::info!("HOME pull cursor={cursor} entries=0");
        return Ok(pull);
    }
    outbox
        .submit(Command::Fence { token: last })
        .map_err(gone)?;
    let deadline = tokio::time::Instant::now() + SETTLE;
    while tokio::time::Instant::now() < deadline {
        match outbox.try_next_event().map_err(gone)? {
            Some(OutboxEvent::Fenced { token }) if token == last => {
                pull.cursor = Some(last);
                break;
            }
            Some(event @ OutboxEvent::Received { .. }) => pull.received.push(event),
            Some(_) => {}
            None => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    tracing::info!(
        "HOME pull cursor={cursor} entries={} messages={} states={} names={} imported={} \
         through={last} settled={}",
        pull.entries,
        pull.messages(),
        pull.states(),
        pull.names.len(),
        pull.imported.len(),
        pull.cursor.is_some()
    );
    Ok(pull)
}

/// `loquax fetch` on a client: pull from the home node, print what is new as
/// a fetch from a propagation node prints it, and a line for each state
/// change of a message sent through the home node. How many were new.
#[allow(clippy::too_many_arguments)]
pub async fn fetch<E: crate::effects::Effects>(
    outbox: &mut impl crate::outbox::Outbox,
    node: &ReticulumNode,
    home: &std::path::Path,
    public_key: &[u8; 64],
    device: &Identity,
    budget: Duration,
    effects: &mut E,
    seen: &mut crate::seen::SeenStore,
) -> Result<usize, crate::fetch::FetchError> {
    use crate::fetch::{take_received, FetchError};
    use crate::outbox::OutboxEvent;
    let pull = pull_into(outbox, node, home, public_key, device, budget)
        .await
        .map_err(|error| FetchError::SyncFailed(error.to_string()))?;
    let mut new = 0;
    for event in &pull.received {
        if let OutboxEvent::Received {
            message_id,
            source,
            title,
            body,
            verified,
            ..
        } = event
        {
            if take_received(seen, effects, message_id, source, title, body, *verified)? {
                new += 1;
            }
        }
    }
    for (id, kind) in &pull.sent {
        if let Some(word) = super::sent_word(*kind) {
            let line = format!("message {} {word}\n", crate::address::to_hex(id));
            effects
                .print(line.as_bytes())
                .map_err(FetchError::SyncFailed)?;
        }
    }
    if let Some(cursor) = pull.cursor {
        save_cursor(home, cursor).map_err(FetchError::SyncFailed)?;
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::fake::FakeOutbox;
    use crate::outbox::{Command, OutboxEvent};
    use leviculum_lxmf::router::MessageState;

    fn entry(seq: u64, kind: u8, packed: Vec<u8>) -> Entry {
        Entry {
            seq,
            kind,
            at: 0,
            packed,
        }
    }

    fn pulled() -> Pulled {
        Pulled {
            entries: vec![
                entry(4, super::super::KIND_RECEIVED, vec![1, 2, 3]),
                entry(5, super::super::KIND_DELIVERED, vec![9; 32]),
                entry(6, super::super::KIND_RETRIES_ENDED, vec![8; 32]),
            ],
            first: 1,
        }
    }

    /// An imported history entry (1150) is kept for the frontend's store,
    /// not handed to the engine, and the cursor still waits for the fence.
    #[tokio::test]
    async fn an_imported_entry_is_kept_for_the_store_and_not_handed_on() {
        let pulled = Pulled {
            entries: vec![entry(7, super::super::KIND_IMPORTED, vec![0x9a, 1])],
            first: 1,
        };
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Fenced { token: 7 }]);
        let pull = take_in(&mut outbox, 6, pulled).await.expect("pull");
        assert_eq!(pull.imported, [vec![0x9a, 1]]);
        assert_eq!(pull.cursor, Some(7));
        assert_eq!((pull.messages(), pull.states()), (0, 0));
        assert_eq!(*outbox.commands.borrow(), [Command::Fence { token: 7 }]);
    }

    /// History is owed until a pull settled (1429): a home with no cursor
    /// file, or one at 0, is a device that never pulled; any saved cursor
    /// past 0 means live mail from here on.
    #[test]
    fn history_is_owed_until_a_pull_settled() {
        let home = tempfile::tempdir().expect("home");
        assert!(history_owed(home.path()));
        save_cursor(home.path(), 0).expect("cursor");
        assert!(history_owed(home.path()));
        save_cursor(home.path(), 3611).expect("cursor");
        assert!(!history_owed(home.path()));
    }

    /// The pull hands everything on in order, fenced, and names the cursor
    /// only once the fence came back: the caller moves it after keeping what
    /// came, so a crash before that pulls the same entries again.
    #[tokio::test]
    async fn the_cursor_is_named_only_behind_the_fence_and_never_saved_here() {
        let home = tempfile::tempdir().expect("home");
        save_cursor(home.path(), 3).expect("cursor");
        let received = OutboxEvent::Received {
            sent_at: 0,
            message_id: [1; 32],
            source: [2; 16],
            title: Vec::new(),
            body: b"x".to_vec(),
            fields: Vec::new(),
            verified: true,
        };
        let mut outbox = FakeOutbox::new(vec![received.clone(), OutboxEvent::Fenced { token: 6 }]);
        let pull = take_in(&mut outbox, 3, pulled()).await.expect("pull");
        assert_eq!(pull.cursor, Some(6));
        assert_eq!(pull.entries, 3);
        assert_eq!(pull.received, [received]);
        assert_eq!((pull.messages(), pull.states()), (1, 2));
        assert_eq!(
            pull.sent,
            [
                ([9; 32], super::super::KIND_DELIVERED),
                ([8; 32], super::super::KIND_RETRIES_ENDED)
            ]
        );
        assert_eq!(
            *outbox.commands.borrow(),
            [
                Command::Ingest {
                    packed: vec![1, 2, 3]
                },
                Command::HomeState {
                    message_id: [9; 32],
                    state: MessageState::Delivered
                },
                Command::Fence { token: 6 },
            ]
        );
        // Nothing here moved the file: that is the caller's, after its store.
        assert_eq!(load_cursor(home.path()), 3);
    }

    /// A sent entry goes to the engine whole, for a bubble on a device that
    /// does not have one; a mark goes as the mark, a heard name as the name,
    /// a petname as the petname (1139), a peer state as the state (1146);
    /// one that does not decode is passed over and still counted into the
    /// cursor.
    #[tokio::test]
    async fn sent_mark_and_name_entries_reach_the_engine() {
        use leviculum_core::Identity;
        let home = Identity::generate(&mut rand_core::OsRng);
        let message = leviculum_lxmf::Message::create(
            [4; 16],
            [5; 16],
            &home,
            1.0,
            Vec::new(),
            b"x".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let mark = wire::Mark {
            flag: wire::MarkFlag::Read,
            ids: vec![[6; 32]],
        };
        let blocked = wire::PeerState {
            blocked: true,
            ..wire::PeerState::default()
        };
        let pulled = Pulled {
            entries: vec![
                entry(1, super::super::KIND_SENT, message.pack()),
                entry(2, super::super::KIND_MARK, wire::mark_request(&mark)),
                entry(3, super::super::KIND_MARK, vec![0x90]),
                entry(
                    4,
                    super::super::KIND_NAME,
                    wire::name_entry(&[5; 16], "Reviewer Berlin"),
                ),
                entry(5, super::super::KIND_NAME, vec![0x90]),
                entry(
                    6,
                    super::super::KIND_PETNAME,
                    wire::petname_entry(&[5; 16], Some("Mum")),
                ),
                entry(
                    7,
                    super::super::KIND_PETNAME,
                    wire::petname_entry(&[4; 16], None),
                ),
                entry(8, super::super::KIND_PETNAME, vec![0x90]),
                entry(
                    9,
                    super::super::KIND_PEERSTATE,
                    wire::peer_state_entry(&[5; 16], &blocked),
                ),
            ],
            first: 1,
        };
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Fenced { token: 9 }]);
        let pull = take_in(&mut outbox, 0, pulled).await.expect("pull");
        assert_eq!(pull.cursor, Some(9));
        assert_eq!(pull.sent, [(message.message_id, super::super::KIND_SENT)]);
        // 1136: the user's own sent message, a mark, a name and a petname
        // are entries, and none of them is a new message or a state change.
        assert_eq!((pull.entries, pull.messages(), pull.states()), (9, 0, 0));
        assert_eq!(pull.names, [([5; 16], "Reviewer Berlin".to_string())]);
        assert_eq!(
            *outbox.commands.borrow(),
            [
                Command::HomeSent {
                    packed: message.pack()
                },
                Command::HomeMark { mark },
                Command::HomeName {
                    destination: [5; 16],
                    name: "Reviewer Berlin".to_string()
                },
                Command::HomePetname {
                    destination: [5; 16],
                    petname: Some("Mum".to_string()),
                    at: 0
                },
                Command::HomePetname {
                    destination: [4; 16],
                    petname: None,
                    at: 0
                },
                Command::HomePeerState {
                    destination: [5; 16],
                    state: blocked,
                    at: 0
                },
                Command::Fence { token: 9 },
            ]
        );
    }

    /// 1599: a hub the home heard reaches the engine as the hub, its name
    /// cleaned again; a broken hub entry is skipped, and neither is a
    /// message or a state change.
    #[tokio::test]
    async fn a_hub_entry_reaches_the_engine_as_the_hub() {
        let hub = crate::ui::hubs::HeardHub {
            hash: [8; 16],
            name: Some("Villa".into()),
            hops: Some(4),
            first: 100,
            last: 200,
            answered: None,
        };
        let mut said = hub.clone();
        said.name = Some("Villa_RRC".into());
        let pulled = Pulled {
            entries: vec![
                entry(3, super::super::KIND_HUB, wire::hub_entry(&said)),
                entry(4, super::super::KIND_HUB, vec![0x90]),
            ],
            first: 1,
        };
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Fenced { token: 4 }]);
        let pull = take_in(&mut outbox, 2, pulled).await.expect("pull");
        assert_eq!((pull.messages(), pull.states()), (0, 0));
        assert_eq!(
            *outbox.commands.borrow(),
            [Command::HomeHub { hub }, Command::Fence { token: 4 }]
        );
    }

    /// 1643: a hub entry with the rooms the home's question named reaches
    /// the engine as the hub and its rooms; one without them as the hub
    /// alone.
    #[tokio::test]
    async fn a_hub_entry_with_rooms_reaches_the_engine_with_them() {
        let hub = crate::ui::hubs::HeardHub {
            hash: [8; 16],
            name: Some("Villa".into()),
            hops: Some(4),
            first: 100,
            last: 200,
            answered: None,
        };
        let rooms = vec!["#lab".to_string(), "#treff".to_string()];
        let pulled = Pulled {
            entries: vec![
                entry(
                    3,
                    super::super::KIND_HUB,
                    wire::hub_entry_rooms(&hub, Some(&rooms)),
                ),
                entry(4, super::super::KIND_HUB, wire::hub_entry(&hub)),
            ],
            first: 1,
        };
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Fenced { token: 4 }]);
        take_in(&mut outbox, 2, pulled).await.expect("pull");
        assert_eq!(
            *outbox.commands.borrow(),
            [
                Command::HomeHub { hub: hub.clone() },
                Command::HomeHubRooms {
                    hub: [8; 16],
                    rooms
                },
                Command::HomeHub { hub },
                Command::Fence { token: 4 },
            ]
        );
    }

    /// 1293: a stalled entry reaches the engine as the message it names,
    /// counted as a state change.
    #[tokio::test]
    async fn a_stalled_entry_reaches_the_engine() {
        let pulled = Pulled {
            entries: vec![entry(3, super::super::KIND_STALLED, vec![7; 32])],
            first: 1,
        };
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Fenced { token: 3 }]);
        let pull = take_in(&mut outbox, 2, pulled).await.expect("pull");
        assert_eq!(pull.sent, [([7; 32], super::super::KIND_STALLED)]);
        assert_eq!(pull.states(), 1);
        assert_eq!(
            *outbox.commands.borrow(),
            [
                Command::HomeStalled {
                    message_id: [7; 32]
                },
                Command::Fence { token: 3 },
            ]
        );
    }

    /// 1409: the home's word of what the propagation node answered reaches
    /// the engine as it was kept; one that does not decode is passed over
    /// and still counted into the cursor.
    #[tokio::test]
    async fn a_pn_status_entry_reaches_the_engine() {
        use leviculum_lxmf::lev_status::MessageStatus;
        let answers = vec![([7; 32], MessageStatus::Collected)];
        let pulled = Pulled {
            entries: vec![
                entry(
                    3,
                    super::super::KIND_PN_STATUS,
                    wire::pn_status_entry(&answers),
                ),
                entry(4, super::super::KIND_PN_STATUS, vec![0x80]),
            ],
            first: 1,
        };
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Fenced { token: 4 }]);
        let pull = take_in(&mut outbox, 2, pulled).await.expect("pull");
        assert_eq!(pull.cursor, Some(4));
        assert_eq!((pull.messages(), pull.states()), (0, 0));
        assert_eq!(
            *outbox.commands.borrow(),
            [
                Command::HomePnStatus { answers },
                Command::Fence { token: 4 },
            ]
        );
    }

    /// A home over TCP in this process that answers `/sync` with an empty
    /// page and, when `groups`, `/group` with `GROUP`; built before groups
    /// it has no handler for `/group`, and its core drops the request, as
    /// the miauhaus home of 2026-10-31 did (1307 pass five).
    async fn fake_home(
        groups: bool,
    ) -> (
        std::sync::Arc<ReticulumNode>,
        ReticulumNode,
        [u8; 64],
        Vec<tempfile::TempDir>,
    ) {
        let (home, phone, key, dirs, _) = fake_home_sending(groups, Sends::Unknown).await;
        (home, phone, key, dirs)
    }

    /// What the fake home does with a `/send`.
    #[derive(Clone, Copy)]
    enum Sends {
        /// No handler: the core drops it.
        Unknown,
        /// Answers with `SENT` as the message's id.
        Answer,
        /// Closes the link the request came on, so its answer is lost
        /// every time: a dead home behind a live path.
        Hang,
    }

    /// [`fake_home`] with `sends` for `/send`, and the count of the
    /// `/send` requests it saw.
    async fn fake_home_sending(
        groups: bool,
        sends: Sends,
    ) -> (
        std::sync::Arc<ReticulumNode>,
        ReticulumNode,
        [u8; 64],
        Vec<tempfile::TempDir>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use leviculum_core::RequestPolicy;
        use leviculum_std::driver::ReticulumNodeBuilder;
        const GROUP: [u8; 16] = [0x6c; 16];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("the port");
        let (home_dir, phone_dir) = (
            tempfile::tempdir().expect("dir"),
            tempfile::tempdir().expect("dir"),
        );
        let mut home = ReticulumNodeBuilder::new()
            .add_tcp_server_listener(listener)
            .storage_path(home_dir.path().to_path_buf())
            .build()
            .await
            .expect("the home's node");
        let mut events = home.take_event_receiver().expect("events");
        home.start().await.expect("start the home");
        let identity = Identity::generate(&mut rand_core::OsRng);
        let public_key = identity.public_key_bytes();
        let destination = super::super::destination(identity).expect("home destination");
        let hash = *destination.hash();
        home.register_destination(destination);
        home.register_request_handler(hash, super::super::SYNC_PATH, RequestPolicy::AllowAll);
        if groups {
            home.register_request_handler(hash, super::super::GROUP_PATH, RequestPolicy::AllowAll);
        }
        if !matches!(sends, Sends::Unknown) {
            home.register_request_handler(hash, super::super::SEND_PATH, RequestPolicy::AllowAll);
        }
        let mut phone = ReticulumNodeBuilder::new()
            .add_tcp_client(port)
            .storage_path(phone_dir.path().to_path_buf())
            .build()
            .await
            .expect("the phone's node");
        phone.start().await.expect("start the phone");
        let home = std::sync::Arc::new(home);
        let answering = std::sync::Arc::clone(&home);
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = std::sync::Arc::clone(&seen);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let leviculum_core::NodeEvent::RequestReceived {
                    link_id,
                    request_id,
                    path,
                    ..
                } = event
                else {
                    continue;
                };
                let answer = match path.as_str() {
                    super::super::SYNC_PATH => wire::sync_answer(&wire::Page::default()),
                    super::super::SEND_PATH => {
                        counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if matches!(sends, Sends::Hang) {
                            let _ = answering.close_link(&link_id).await;
                            continue;
                        }
                        wire::send_answer(&SENT)
                    }
                    _ => wire::group_answer(&GROUP),
                };
                let _ = answering
                    .send_response(&link_id, &request_id, &answer)
                    .await;
            }
        });
        for _ in 0..20 {
            let _ = home.announce_destination(&hash, None).await;
            if phone.has_path(&hash) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        (home, phone, public_key, vec![home_dir, phone_dir], seen)
    }

    /// A home whose node answers `/pages`, `/bookmark` and `/sync` with
    /// the real [`super::super::server::answer`] over its own store, on
    /// TCP; `devices` admitted. The store already holds `pages` NomadNet
    /// nodes it heard. The home's node, the phone's node, the home's key.
    #[cfg(feature = "history")]
    async fn real_home(
        devices: &[[u8; 16]],
        pages: u32,
        names: u32,
    ) -> (
        std::sync::Arc<ReticulumNode>,
        ReticulumNode,
        [u8; 64],
        Vec<tempfile::TempDir>,
    ) {
        use leviculum_core::RequestPolicy;
        use leviculum_std::driver::ReticulumNodeBuilder;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("the port");
        let (home_dir, phone_dir, store_dir) = (
            tempfile::tempdir().expect("dir"),
            tempfile::tempdir().expect("dir"),
            tempfile::tempdir().expect("dir"),
        );
        let mut store =
            crate::store::Store::open_in(store_dir.path(), [0x11; 16]).expect("the store");
        for (i, device) in devices.iter().enumerate() {
            let token = [u8::try_from(i).expect("few devices"); 16];
            store.issue_token(&token, "device", 1).expect("token");
            store
                .redeem_token(&token, device, 2, 3600)
                .expect("admitted");
        }
        for i in 0..pages {
            let mut dest = [0u8; 16];
            dest[..4].copy_from_slice(&i.to_be_bytes());
            store
                .page_heard(&lnomad::discovery::DiscoveredNode {
                    dest_hash: dest,
                    name: Some(alloc::format!("NomadNet node {i}")),
                    first_seen: 1_800_000_000,
                    last_seen: 1_800_000_000,
                    hops: Some(3),
                })
                .expect("heard");
        }
        for i in 0..names {
            let mut address = [0u8; 16];
            address[..4].copy_from_slice(&i.to_be_bytes());
            address[15] = 0xaa;
            store
                .name_heard(&super::super::pages::NameRow {
                    address,
                    name: alloc::format!("Stranger {i}"),
                    first_seen: 1_800_000_000,
                    last_seen: 1_800_000_000,
                    hops: Some(2),
                })
                .expect("heard");
        }
        let mut home = ReticulumNodeBuilder::new()
            .add_tcp_server_listener(listener)
            .storage_path(home_dir.path().to_path_buf())
            .build()
            .await
            .expect("the home's node");
        let mut events = home.take_event_receiver().expect("events");
        home.start().await.expect("start the home");
        let identity = Identity::generate(&mut rand_core::OsRng);
        let public_key = identity.public_key_bytes();
        let signing: &'static Identity = Box::leak(Box::new(identity.clone()));
        let destination = super::super::destination(identity).expect("home destination");
        let hash = *destination.hash();
        home.register_destination(destination);
        for path in [
            super::super::SYNC_PATH,
            super::super::PAGES_PATH,
            super::super::BOOKMARK_PATH,
            super::super::ABOUT_PATH,
        ] {
            home.register_request_handler(hash, path, RequestPolicy::AllowAll);
        }
        let mut phone = ReticulumNodeBuilder::new()
            .add_tcp_client(port)
            .storage_path(phone_dir.path().to_path_buf())
            .build()
            .await
            .expect("the phone's node");
        phone.start().await.expect("start the phone");
        let home = std::sync::Arc::new(home);
        let answering = std::sync::Arc::clone(&home);
        tokio::spawn(async move {
            let serving = super::super::server::Home {
                identity: signing,
                address: [0x11; 16],
                token_ttl_secs: 3600,
                cap_bytes: u64::MAX,
                handover: None,
                groups: None,
                shown: Default::default(),
            };
            while let Some(event) = events.recv().await {
                let leviculum_core::NodeEvent::RequestReceived {
                    link_id,
                    request_id,
                    path,
                    data,
                    ..
                } = event
                else {
                    continue;
                };
                let remote = answering
                    .get_remote_identity(&link_id)
                    .map(|identity| *identity.hash());
                let answer =
                    super::super::server::answer(&mut store, &serving, &path, &data, remote, 100.0);
                let sent = answering
                    .send_response(&link_id, &request_id, &answer.bytes)
                    .await;
                if let Err(leviculum_std::Error::Request(
                    leviculum_core::RequestError::PayloadTooLarge,
                )) = sent
                {
                    let _ = answering
                        .send_response_resource_with(
                            &link_id,
                            &request_id,
                            &answer.bytes,
                            super::super::ANSWERS_COMPRESSED,
                        )
                        .await;
                }
            }
        });
        for _ in 0..20 {
            let _ = home.announce_destination(&hash, None).await;
            if phone.has_path(&hash) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        (
            home,
            phone,
            public_key,
            vec![home_dir, phone_dir, store_dir],
        )
    }

    /// 1642 over a real link: `/about` answers an admitted device with
    /// the mailbox's devices, the asker among them, and refuses one that is
    /// not admitted.
    #[cfg(feature = "history")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn about_names_the_devices_over_a_link_and_refuses_a_stranger() {
        let phone_device = Identity::generate(&mut rand_core::OsRng);
        let (_home, phone, key, _dirs) = real_home(&[*phone_device.hash()], 0, 0).await;
        let budget = Duration::from_secs(60);
        let told = about(&phone, &key, &phone_device, budget)
            .await
            .expect("the mailbox's word");
        assert_eq!(told.version, crate::VERSION);
        assert_eq!(told.devices.len(), 1);
        assert_eq!(told.devices[0].device, *phone_device.hash());
        let stranger = Identity::generate(&mut rand_core::OsRng);
        assert_eq!(
            about(&phone, &key, &stranger, budget).await.err(),
            Some(HomeError::Refused(wire::Status::NotAdmitted))
        );
    }

    /// 1579 over a real link: a device's first `/pages` brings the 2000
    /// pages the home heard, and a bookmark another device set; the next
    /// ask after its stamp brings only what changed since, the other
    /// device's clearing as a tombstone. The full list's cost on TCP is
    /// said in the log (`PAGES full`).
    #[cfg(feature = "history")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pages_and_a_bookmark_reach_a_second_device_over_a_link() {
        use super::super::pages::BookmarkRow;
        let laptop = Identity::generate(&mut rand_core::OsRng);
        let phone_device = Identity::generate(&mut rand_core::OsRng);
        let (_home, phone, key, _dirs) =
            real_home(&[*laptop.hash(), *phone_device.hash()], 2000, 0).await;
        let budget = Duration::from_secs(60);
        let set = BookmarkRow {
            url: "00000001000000000000000000000000:/page/index.mu".into(),
            title: "Node one".into(),
            at: 1_800_000_000_000,
            deleted: false,
        };
        bookmark(&phone, &key, &laptop, &set, budget)
            .await
            .expect("the laptop's bookmark");
        let started = std::time::Instant::now();
        let full = pages(&phone, &key, &phone_device, 0, budget)
            .await
            .expect("the full list");
        let took = started.elapsed();
        let bytes = super::super::pages::pages_answer(&full).len();
        eprintln!(
            "PAGES full rows={} bytes={bytes} took_ms={}",
            full.rows.len(),
            took.as_millis()
        );
        assert_eq!(full.nodes().count(), 2000);
        assert_eq!(full.bookmarks().collect::<Vec<_>>(), [&set]);
        let cleared = BookmarkRow {
            at: set.at + 1,
            deleted: true,
            ..set.clone()
        };
        bookmark(&phone, &key, &laptop, &cleared, budget)
            .await
            .expect("the laptop's clearing");
        let delta = pages(&phone, &key, &phone_device, full.stamp, budget)
            .await
            .expect("the delta");
        assert_eq!(delta.nodes().count(), 0, "no page changed");
        assert_eq!(delta.bookmarks().collect::<Vec<_>>(), [&cleared]);
        let stranger = Identity::generate(&mut rand_core::OsRng);
        assert_eq!(
            pages(&phone, &key, &stranger, 0, budget).await,
            Err(HomeError::Refused(Status::NotAdmitted))
        );
    }

    /// 1643 over a real link: a mailbox that heard 2000 names hands them
    /// all in one pull (`/pages` page by page until no more follow), and a
    /// phone that folds the answer names every one of those strangers,
    /// none of whom it heard itself. The delta's cost on TCP is said in the
    /// log (`NAMES full`). Control: the next ask after the stamp brings
    /// nothing.
    #[cfg(feature = "history")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_phone_names_the_two_thousand_strangers_its_mailbox_heard_after_one_pull() {
        let phone_device = Identity::generate(&mut rand_core::OsRng);
        let (_home, phone, key, _dirs) = real_home(&[*phone_device.hash()], 0, 2000).await;
        let budget = Duration::from_secs(60);
        let started = std::time::Instant::now();
        let full = pages(&phone, &key, &phone_device, 0, budget)
            .await
            .expect("the full list");
        let took = started.elapsed();
        let bytes = super::super::pages::pages_answer(&full).len();
        eprintln!(
            "NAMES full rows={} bytes={bytes} took_ms={}",
            full.rows.len(),
            took.as_millis()
        );
        assert_eq!(full.names().count(), 2000);
        assert_eq!(full.nodes().count(), 0);
        let mut model = crate::ui::UiModel::<crate::ui::tests::Field>::new(&[]);
        let mut stranger = [0u8; 16];
        stranger[..4].copy_from_slice(&1234u32.to_be_bytes());
        stranger[15] = 0xaa;
        assert_eq!(model.names.announced(&stranger), None, "never heard here");
        crate::ui::update(
            &mut model,
            crate::ui::UiEvent::Browse(crate::ui::browse::BrowseEvent::Home(full.clone())),
        );
        assert_eq!(model.names.heard_count(), 2000);
        assert_eq!(model.names.announced(&stranger), Some("Stranger 1234"));
        let again = pages(&phone, &key, &phone_device, full.stamp, budget)
            .await
            .expect("the delta");
        assert!(again.rows.is_empty(), "nothing new: {again:?}");
    }

    /// The message id the fake home gives every `/send` it answers.
    const SENT: [u8; 32] = [0x5e; 32];

    /// 1371: a send is one ask with one retry policy. A home whose answer
    /// is lost every time is asked `ATTEMPTS` (3) times in all, not the
    /// nine the old loop around the ask would have made, and the error
    /// is the lost answer's. Control: a live home is asked once and
    /// gives its id.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dead_home_is_asked_three_times_and_a_live_one_once() {
        let device = Identity::generate(&mut rand_core::OsRng);
        let submission = wire::Submission {
            client_id: [3; 16],
            destination: [0x5a; 16],
            title: Vec::new(),
            content: b"hallo".to_vec(),
            fields: Vec::new(),
        };
        let budget = Duration::from_secs(30);

        let (_home, phone, key, _dirs, seen) = fake_home_sending(false, Sends::Hang).await;
        let started = tokio::time::Instant::now();
        let sent = send(&phone, &key, &device, &submission, budget).await;
        assert!(
            matches!(sent, Err(HomeError::Transfer(_))),
            "the lost answer's error: {sent:?}"
        );
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(super::super::ask::ATTEMPTS)
        );
        assert_eq!(super::super::ask::ATTEMPTS, 3);
        assert!(
            started.elapsed() < budget,
            "gave up on the attempts, not the budget: {:?}",
            started.elapsed()
        );

        let (_home, phone, key, _dirs, seen) = fake_home_sending(false, Sends::Answer).await;
        let sent = send(&phone, &key, &device, &submission, budget).await;
        assert_eq!(sent, Ok(SENT));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// 1307 pass five, Lew's phone on the old miauhaus home: `/group` is
    /// met by silence while `/sync` on the same link is answered, twice,
    /// so the client says the home does not know groups instead of a
    /// timeout after the whole budget. Control: a home with the handler
    /// answers the group's address on the first ask.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_home_silent_on_group_but_answering_sync_cannot_host_groups() {
        let device = Identity::generate(&mut rand_core::OsRng);
        let request = wire::GroupRequest {
            client_id: [7; 16],
            name: "Familie".to_string(),
            members: vec![([5; 16], "Anna".to_string())],
        };
        let wait = Duration::from_secs(2);
        let budget = Duration::from_secs(30);

        let (_home, phone, key, _dirs) = fake_home(false).await;
        let started = tokio::time::Instant::now();
        let made = group_waiting(&phone, &key, &device, &request, budget, wait).await;
        assert_eq!(made, Err(HomeError::Unsupported(super::super::GROUP_PATH)));
        assert!(
            started.elapsed() < budget,
            "said before the budget ran out: {:?}",
            started.elapsed()
        );

        let (_home, phone, key, _dirs) = fake_home(true).await;
        let made = group_waiting(&phone, &key, &device, &request, budget, wait).await;
        assert_eq!(made, Ok([0x6c; 16]));
    }

    /// Positive control for the above: without the fence there is no cursor
    /// to save, so the next pull asks for the same entries.
    #[tokio::test(start_paused = true)]
    async fn without_the_fence_there_is_no_cursor_to_save() {
        let mut outbox = FakeOutbox::new(vec![OutboxEvent::Fenced { token: 5 }]);
        let pull = take_in(&mut outbox, 3, pulled()).await.expect("pull");
        assert_eq!(pull.cursor, None);
        assert_eq!(pull.entries, 3);
    }
}
