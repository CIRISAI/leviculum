//! How a client talks to its home node: a link to `loquax.home`, identified
//! with the device's own identity, one request, the answer, the link closed.
//! Over the driver's own link API (`leviculum_std::ReticulumNode`), next to
//! the engine rather than inside it: the engine runs under the core lock and
//! may not wait, and a pull is nothing but waiting.

use std::time::Duration;

use leviculum_core::{DestinationHash, Identity, LinkId};
use leviculum_std::ReticulumNode;

use super::wire::{self, Entry, Status};

/// How long after a path request the next one goes while no path has come:
/// first the reference's wait between a path request and the next attempt
/// (`leviculum_lxmf::router::PATH_REQUEST_WAIT_MS`), doubled on each one
/// unanswered up to [`PATH_RETRY_MAX`]. A mailbox behind a third party's
/// plain port whose announce has not reached the entry node yet is found by
/// a later request, or by its next announce (1196).
const PATH_RETRY: Duration = Duration::from_millis(leviculum_lxmf::router::PATH_REQUEST_WAIT_MS);
/// The longest wait between two path requests within one attempt.
const PATH_RETRY_MAX: Duration = Duration::from_secs(60);
/// How often a waiting path is looked for.
const PATH_POLL: Duration = Duration::from_millis(100);

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
        }
    }
}

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
    let deadline = tokio::time::Instant::now() + budget;
    let link = open(node, public_key, device, deadline).await?;
    let answer = request(
        node,
        &link,
        super::JOIN_PATH,
        &wire::join_request(token, name),
        deadline,
    )
    .await;
    let _ = node.close_link(&link).await;
    match wire::read_status(&answer?).map_err(|_| HomeError::Malformed)? {
        Status::Ok => Ok(()),
        status => Err(HomeError::Refused(status)),
    }
}

/// Hand one message to the home node to send: link, identify as `device`,
/// `/send`. The message id the home node gave it, also when the same
/// `client_id` was handed in before.
pub async fn send(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    submission: &wire::Submission,
    budget: Duration,
) -> Result<[u8; 32], HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let link = open(node, public_key, device, deadline).await?;
    let answer = request(
        node,
        &link,
        super::SEND_PATH,
        &wire::send_request(submission),
        deadline,
    )
    .await;
    let _ = node.close_link(&link).await;
    wire::read_send_answer(&answer?)
        .map_err(|_| HomeError::Malformed)?
        .map_err(HomeError::Refused)
}

/// [`send`], asked again with the same submission when the answer was lost
/// (the request or its answer failed, or ran out of time), up to three times
/// within `budget`: the home node answers a client id it has seen with the
/// message it made then, so a retry never sends twice.
pub async fn send_retrying(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    submission: &wire::Submission,
    budget: Duration,
) -> Result<[u8; 32], HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match send(node, public_key, device, submission, left).await {
            Err(HomeError::Transfer(detail)) if attempt < 3 && !left.is_zero() => {
                tracing::info!("HOME send attempt {attempt} lost its answer: {detail}");
            }
            Err(HomeError::Timeout) if attempt < 3 && !left.is_zero() => {
                tracing::info!("HOME send attempt {attempt} ran out of time");
            }
            other => return other,
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(HomeError::Timeout);
        }
    }
}

/// Hand the home node one mark: link, identify as `device`, `/mark`,
/// asked again up to three times when the answer was lost, as
/// [`send_retrying`] does: the home node keeps a mark it has once.
pub async fn mark(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    mark: &wire::Mark,
    budget: Duration,
) -> Result<(), HomeError> {
    let data = wire::mark_request(mark);
    ask_retrying(node, public_key, device, super::MARK_PATH, &data, budget).await
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
            Ok(bytes) => {
                return match wire::read_status(&bytes).map_err(|_| HomeError::Malformed)? {
                    Status::Ok => Ok(()),
                    status => Err(HomeError::Refused(status)),
                }
            }
            Err(HomeError::Transfer(_) | HomeError::Timeout)
                if attempt < 3 && tokio::time::Instant::now() < deadline => {}
            Err(error) => return Err(error),
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

/// Pull everything after `cursor`, page by page over one link.
pub async fn pull(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    cursor: u64,
    budget: Duration,
) -> Result<Pulled, HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let link = open(node, public_key, device, deadline).await?;
    let pulled = pages(node, &link, cursor, deadline).await;
    let _ = node.close_link(&link).await;
    pulled
}

async fn pages(
    node: &ReticulumNode,
    link: &LinkId,
    mut cursor: u64,
    deadline: tokio::time::Instant,
) -> Result<Pulled, HomeError> {
    let mut pulled = Pulled::default();
    loop {
        let answer = request(
            node,
            link,
            super::SYNC_PATH,
            &wire::sync_request(cursor),
            deadline,
        )
        .await?;
        let page = wire::read_sync_answer(&answer)
            .map_err(|_| HomeError::Malformed)?
            .map_err(HomeError::Refused)?;
        pulled.first = page.first;
        let last = page.entries.last().map(|entry| entry.seq);
        pulled.entries.extend(page.entries);
        match (page.more, last) {
            (true, Some(last)) if last > cursor => cursor = last,
            _ => return Ok(pulled),
        }
    }
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
    let mut asked: Option<tokio::time::Instant> = None;
    let mut retry = PATH_RETRY;
    let mut requests = 0u32;
    while !node.has_path(&destination) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            tracing::info!(
                "HOME no path to the mailbox after {requests} path request(s); the next \
                 attempt asks again"
            );
            return Err(HomeError::NoPath);
        }
        if let Some(at) = asked {
            if now.duration_since(at) >= retry {
                retry = (retry * 2).min(PATH_RETRY_MAX);
                asked = None;
            }
        }
        if asked.is_none() {
            asked = Some(now);
            requests += 1;
            if requests > 1 {
                tracing::info!(
                    "HOME looking for the mailbox: path request {requests}, the next in {} s",
                    retry.as_secs()
                );
            }
            node.request_path(&destination)
                .await
                .map_err(|e| HomeError::NoLink(e.to_string()))?;
        }
        tokio::time::sleep(PATH_POLL).await;
    }
    // Rule 3 (1196): a link over a long path may take the transport's own
    // establishment time, per hop as the reference reckons it
    // (`ESTABLISHMENT_TIMEOUT_PER_HOP`, `reference/Reticulum/RNS/Link.py:284`),
    // however short the caller's budget was set on a TCP round trip's
    // reckoning.
    let hops = u64::from(node.hops_to(&destination).unwrap_or(1).max(1));
    let deadline = deadline.max(
        tokio::time::Instant::now()
            + Duration::from_millis(
                leviculum_core::constants::ESTABLISHMENT_TIMEOUT_PER_HOP_MS.saturating_mul(hops),
            ),
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

/// One request on `link` and its answer's bytes.
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
        Err(_) => Err(HomeError::Timeout),
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
                    })
                    .map_err(gone)?,
                Err(_) => tracing::warn!("HOME pull: entry {} is no petname", entry.seq),
            }
            continue;
        }
        if entry.kind == super::KIND_PEERSTATE {
            match wire::read_peer_state_entry(&entry.packed) {
                Ok((destination, state)) => outbox
                    .submit(Command::HomePeerState { destination, state })
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
        if entry.kind == super::KIND_IMPORTED {
            pull.imported.push(entry.packed.clone());
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

    /// The pull hands everything on in order, fenced, and names the cursor
    /// only once the fence came back: the caller moves it after keeping what
    /// came, so a crash before that pulls the same entries again.
    #[tokio::test]
    async fn the_cursor_is_named_only_behind_the_fence_and_never_saved_here() {
        let home = tempfile::tempdir().expect("home");
        save_cursor(home.path(), 3).expect("cursor");
        let received = OutboxEvent::Received {
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
                    petname: Some("Mum".to_string())
                },
                Command::HomePetname {
                    destination: [4; 16],
                    petname: None
                },
                Command::HomePeerState {
                    destination: [5; 16],
                    state: blocked
                },
                Command::Fence { token: 9 },
            ]
        );
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
