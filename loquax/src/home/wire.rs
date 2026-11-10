//! The requests a client sends the home node, and their answers, as
//! msgpack values on an RNS link (`docs/src/concepts/loquax-home-node.md`,
//! "The client protocol").
//!
//! ```text
//! /join  [token: bin16, name: str]           -> [status]
//! /sync  [cursor: uint, (page: uint, (fold: str))]
//!                                             -> [status, first: uint, more: bool,
//!                                                 [[seq: uint, kind: uint, at: int, packed: bin], ...]]
//! /send  [client_id: bin16, destination: bin16, title: bin, content: bin,
//!         [[key: int, value: bin], ...]]      -> [status, id: bin32]
//! /mark  [flag: uint, [id: bin32, ...]]       -> [status]
//! /mark  [2, [id: bin32, ...], read_at: uint] -> [status]
//! /watch [cursor: uint]                       -> [status, newest: uint]
//! /key   [peer: bin16, public_key: bin64]     -> [status]
//! /move  [token: bin16, cursor: uint]         -> [status, private_key: bin64, ratchets: bin,
//!                                                 known: bin, name: str, first: uint, more: bool,
//!                                                 [[seq, kind, id: bin32, at, packed], ...]]
//! /moved [token: bin16, cursor: uint]         -> [status]
//! /group [client_id: bin16, name: str,
//!         [[address: bin16, name: str], ...]] -> [status, group: bin16]
//! /groupadd [client_id: bin16, group: bin16,
//!         [[address: bin16, name: str], ...]] -> [status, group: bin16]
//!
//! wake   ["pull", newest: uint]               home node to client, on the
//!                                             link that asked /watch
//! ```
//!
//! `/watch` keeps the link it came on: the home node sends a wake over it,
//! as a channel message, whenever the archive grew, at most one per
//! [`super::wake::COALESCE_MS`] per link. A wake carries no content, only
//! the newest sequence, and the client pulls with `/sync` when that is past
//! its cursor ([`super::watch`]). `newest` in the answer is the same number
//! at the time of the request, 0 for an empty archive.
//!
//! A mark's flag is 0 for read and 1 for deleted ([`MarkFlag`]). The home
//! node keeps the request's bytes as the archive entry, so every client
//! reads the same mark the way the home node did.
//!
//! Flag 2 is seen (1234, `docs/src/concepts/loquax-read-receipts.md`): the
//! bubbles were on a device's screen, and the third element is the
//! reader's clock when the first of them was. The home node signs the read
//! receipts for them, since the devices hold no identity, and keeps as the
//! archive entry a seen mark of only the ids no device saw before, so the
//! first device to see a message sets its read time and every other one
//! learns it was seen. A home node from before refuses flag 2 as
//! malformed, and no receipt goes.
//!
//! A field's value is its raw msgpack, carried as bytes, so the home node
//! signs exactly what the client composed.
//!
//! `/sync`'s second element is optional (1363): the most bytes the answer
//! may take, for a client on a narrow medium. A LoRa client asks
//! [`super::ask::LORA_PAGE_BYTES`], so one page is one packet on a link of
//! the default MTU and never a resource. The home cuts the page to it, at
//! least one entry, and says `more`; a home from before reads the cursor
//! alone and answers its own page.
//!
//! Its third element is optional too (1439): the device's capability word,
//! what it folds ([`Fold`]). A board says `board` and is served only the
//! entries `board::fold` keeps, with `left` counting only those; a home
//! from before reads the first two and serves everything.
//!
//! The status is the first element of every answer, so a refusal decodes
//! the same way whatever was asked. `first` is the oldest sequence the
//! archive still holds: a client whose cursor is older than `first - 1`
//! lost entries to the cap and can say so.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::msgpack;

/// One archive entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its sequence: every entry after another has a larger one.
    pub seq: u64,
    /// What it records ([`super::KIND_RECEIVED`]).
    pub kind: u8,
    /// When the home node recorded it, unix seconds.
    pub at: i64,
    /// The LXMF message as it came, signature and all.
    pub packed: Vec<u8>,
}

/// One message a client hands the home node to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The client's own id for it: the same id handed in again is the same
    /// message.
    pub client_id: [u8; 16],
    pub destination: [u8; 16],
    pub title: Vec<u8>,
    pub content: Vec<u8>,
    pub fields: Vec<leviculum_lxmf::Field>,
}

/// What a mark says of its messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkFlag {
    /// Read on one device: read on all.
    Read,
    /// Deleted on one device: deleted on all.
    Deleted,
    /// On a device's screen (1234): the home node owes their senders a
    /// read receipt, at the reader's clock `at`, unix seconds.
    Seen { at: u32 },
    /// On a device (1462): the home node owes their senders a device
    /// receipt (`lev` `0x07`) at the device's clock `at`, unix seconds,
    /// when the message took it. The first device's mark wins.
    Arrived { at: u32 },
}

/// The code of [`MarkFlag::Seen`].
const SEEN_CODE: u64 = 2;

/// The code of [`MarkFlag::Arrived`].
const ARRIVED_CODE: u64 = 3;

/// The bytes every arrived mark begins with, request or archive entry.
pub const ARRIVED_MARK_PREFIX: [u8; 2] = [0x93, ARRIVED_CODE as u8];

/// The bytes every seen mark begins with, request or archive entry: an
/// array of three and the flag. The store finds the seen marks by them.
pub const SEEN_MARK_PREFIX: [u8; 2] = [0x93, SEEN_CODE as u8];

impl MarkFlag {
    fn code(self) -> u64 {
        match self {
            Self::Read => 0,
            Self::Deleted => 1,
            Self::Seen { .. } => SEEN_CODE,
            Self::Arrived { .. } => ARRIVED_CODE,
        }
    }

    /// The word logs use.
    pub fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Deleted => "deleted",
            Self::Seen { .. } => "seen",
            Self::Arrived { .. } => "arrived",
        }
    }
}

/// One `/mark`: a flag over one or more messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub flag: MarkFlag,
    pub ids: Vec<[u8; 32]>,
}

/// The most message ids one mark carries: a conversation of unread
/// messages opened at once fits in one packet's worth of requests at most a
/// few times over, and a mark larger than this is split by the client.
pub const MARK_IDS_MAX: usize = 64;

/// One `/sync` answer's worth of the archive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// Whether entries after the last one here wait for the next request.
    pub more: bool,
    /// The oldest sequence the archive holds; 0 when it is empty.
    pub first: u64,
    /// How many entries wait after this page (1415): a fifth element the
    /// home sends only to a client that asked a page bound, which is a
    /// client from 1363 on that reads it; one that asked none gets the
    /// four elements it always read. What makes "pulled N of M" on the
    /// board's screen.
    pub left: Option<u64>,
}

/// What `left` is while the home cuts a page to its bound: the longest
/// number msgpack writes, so the cut leaves room for the real count.
pub const LEFT_UNCOUNTED: u64 = u64::MAX;

/// How a request went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The link was not identified, so there is no device to check.
    NotIdentified,
    /// The device is not admitted, or no longer.
    NotAdmitted,
    /// The admission token is unknown or used.
    BadToken,
    /// The request did not decode.
    Malformed,
    /// The home node could not answer (its store failed).
    Failed,
    /// A `/moved` came while the archive held entries after its cursor:
    /// the new device takes them with `/move` first (1281).
    Behind,
}

impl Status {
    fn code(self) -> u64 {
        match self {
            Self::Ok => 0,
            Self::NotIdentified => 1,
            Self::NotAdmitted => 2,
            Self::BadToken => 3,
            Self::Malformed => 4,
            Self::Failed => 5,
            Self::Behind => 6,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Ok,
            1 => Self::NotIdentified,
            2 => Self::NotAdmitted,
            3 => Self::BadToken,
            4 => Self::Malformed,
            5 => Self::Failed,
            6 => Self::Behind,
            _ => return None,
        })
    }
}

impl core::fmt::Display for Status {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::NotIdentified => "the mailbox did not learn who this device is",
            Self::NotAdmitted => "this device is not admitted to the mailbox (or was revoked)",
            Self::BadToken => "the mailbox does not know this admission code, or it was used",
            Self::Malformed => "the mailbox could not read the request",
            Self::Failed => "the mailbox could not answer",
            Self::Behind => "the mailbox holds newer entries than the move took",
        })
    }
}

/// Why an answer did not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed;

impl From<msgpack::Error> for Malformed {
    fn from(_: msgpack::Error) -> Self {
        Self
    }
}

pub fn join_request(token: &[u8; 16], name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, token);
    msgpack::string(&mut out, name);
    out
}

pub fn read_join_request(data: &[u8]) -> Result<([u8; 16], alloc::string::String), Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let token = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let name = msgpack::read_str(data, &mut p)?.into();
    Ok((token, name))
}

pub fn status_answer(status: Status) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 1);
    msgpack::uint(&mut out, status.code());
    out
}

pub fn read_status(data: &[u8]) -> Result<Status, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? < 1 {
        return Err(Malformed);
    }
    Status::from_code(msgpack::read_uint(data, &mut p)?).ok_or(Malformed)
}

pub fn sync_request(cursor: u64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 1);
    msgpack::uint(&mut out, cursor);
    out
}

pub fn read_sync_request(data: &[u8]) -> Result<u64, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? < 1 {
        return Err(Malformed);
    }
    Ok(msgpack::read_uint(data, &mut p)?)
}

/// A `/sync` that asks pages of at most `page_bytes` encoded bytes (1363).
pub fn sync_request_paged(cursor: u64, page_bytes: u64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::uint(&mut out, cursor);
    msgpack::uint(&mut out, page_bytes);
    out
}

/// A `/sync` as the home reads it: the cursor, and the page's bound when
/// the client asked one.
pub fn read_sync_paged(data: &[u8]) -> Result<(u64, Option<u64>), Malformed> {
    let mut p = 0;
    let len = msgpack::array_len(data, &mut p)?;
    if len < 1 {
        return Err(Malformed);
    }
    let cursor = msgpack::read_uint(data, &mut p)?;
    let page = match len {
        1 => None,
        _ => Some(msgpack::read_uint(data, &mut p)?),
    };
    Ok((cursor, page))
}

/// What a device folds, said as the third element of its `/sync` (1439):
/// the home serves it only the entries it keeps and counts only those in
/// `left`, so "Fetched N of M" is M entries this device will get. A
/// client that says nothing, or a word this home does not know, gets the
/// whole archive, as every client from before 1439 does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// Every entry: the phone and the desktop keep the whole archive.
    All,
    /// What `board::fold` keeps: a message received or sent, a mark, a
    /// name; nothing imported, no state the board draws no bubble for,
    /// and no entry over [`BOARD_ENTRY_MAX`], which is a photo or a file
    /// the board neither shows nor holds.
    Board,
}

/// The kinds a [`Fold::Board`] device keeps: the ones `board::fold` reads
/// (`loquax/src/home/board.rs`).
pub const BOARD_KINDS: [u8; 4] = [
    super::KIND_RECEIVED,
    super::KIND_SENT,
    super::KIND_MARK,
    super::KIND_NAME,
];

/// The longest entry a [`Fold::Board`] device is served, in packed bytes:
/// a text with its signature, fields and a small picture fits; a photo
/// the board has no screen for does not.
pub const BOARD_ENTRY_MAX: usize = 16 * 1024;

/// The capability word of [`Fold::Board`] on the wire.
pub const FOLD_BOARD: &str = "board";

impl Fold {
    /// The word on the wire, `None` for [`Fold::All`], which says nothing.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Board => Some(FOLD_BOARD),
        }
    }

    /// The fold a word names; an unknown word folds nothing away.
    pub fn from_word(word: &str) -> Self {
        match word {
            FOLD_BOARD => Self::Board,
            _ => Self::All,
        }
    }

    /// Whether a device that folds so is served an entry of `kind` whose
    /// packed bytes are `len` long.
    pub fn keeps(self, kind: u8, len: usize) -> bool {
        match self {
            Self::All => true,
            Self::Board => BOARD_KINDS.contains(&kind) && len <= BOARD_ENTRY_MAX,
        }
    }
}

/// A `/sync` as a device that folds asks it (1439): the cursor, the page
/// bound, and the capability word when the fold has one.
pub fn sync_request_folded(cursor: u64, page_bytes: u64, fold: Fold) -> Vec<u8> {
    let Some(word) = fold.word() else {
        return sync_request_paged(cursor, page_bytes);
    };
    let mut out = Vec::new();
    msgpack::array(&mut out, 3);
    msgpack::uint(&mut out, cursor);
    msgpack::uint(&mut out, page_bytes);
    msgpack::string(&mut out, word);
    out
}

/// A `/sync` as the home reads it from 1439 on: the cursor, the page bound
/// when asked, and the device's fold, [`Fold::All`] when it said none.
pub fn read_sync_asked(data: &[u8]) -> Result<(u64, Option<u64>, Fold), Malformed> {
    let mut p = 0;
    let len = msgpack::array_len(data, &mut p)?;
    if len < 1 {
        return Err(Malformed);
    }
    let cursor = msgpack::read_uint(data, &mut p)?;
    let page = match len {
        1 => None,
        _ => Some(msgpack::read_uint(data, &mut p)?),
    };
    let fold = match len {
        0..=2 => Fold::All,
        _ => Fold::from_word(msgpack::read_str(data, &mut p)?),
    };
    Ok((cursor, page, fold))
}

/// Cut `page` from its end until its answer takes at most `bytes`, one
/// entry always kept, `more` set when one went: what a client that asked
/// a bound gets.
pub fn fit_page(page: &mut Page, bytes: usize) {
    let mut len = sync_answer(page).len();
    while len > bytes && page.entries.len() > 1 {
        if let Some(entry) = page.entries.pop() {
            len = len.saturating_sub(entry_len(&entry));
        }
        // An upper bound: the entries' array header only shrinks.
        page.more = true;
    }
}

/// What one entry takes in a `/sync` answer.
fn entry_len(entry: &Entry) -> usize {
    let mut out = Vec::new();
    entry_out(&mut out, entry);
    out.len()
}

fn entry_out(out: &mut Vec<u8>, entry: &Entry) {
    msgpack::array(out, 4);
    msgpack::uint(out, entry.seq);
    msgpack::uint(out, u64::from(entry.kind));
    msgpack::int(out, entry.at);
    msgpack::bin(out, &entry.packed);
}

pub fn watch_request(cursor: u64) -> Vec<u8> {
    sync_request(cursor)
}

pub fn read_watch_request(data: &[u8]) -> Result<u64, Malformed> {
    read_sync_request(data)
}

pub fn watch_answer(newest: u64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::uint(&mut out, Status::Ok.code());
    msgpack::uint(&mut out, newest);
    out
}

/// A `/watch` answer: the newest sequence, or the status that refused it.
pub fn read_watch_answer(data: &[u8]) -> Result<Result<u64, Status>, Malformed> {
    let status = read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    Ok(Ok(msgpack::read_uint(data, &mut p)?))
}

/// The word a wake starts with.
const WAKE_WORD: &str = "pull";

/// A wake: pull, the archive's newest sequence is `newest`.
pub fn wake(newest: u64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::string(&mut out, WAKE_WORD);
    msgpack::uint(&mut out, newest);
    out
}

/// A wake's newest sequence. Anything else on the link is malformed.
pub fn read_wake(data: &[u8]) -> Result<u64, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 || msgpack::read_str(data, &mut p)? != WAKE_WORD {
        return Err(Malformed);
    }
    Ok(msgpack::read_uint(data, &mut p)?)
}

pub fn sync_answer(page: &Page) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 4 + usize::from(page.left.is_some()));
    msgpack::uint(&mut out, Status::Ok.code());
    msgpack::uint(&mut out, page.first);
    msgpack::bool(&mut out, page.more);
    msgpack::array(&mut out, page.entries.len());
    for entry in &page.entries {
        entry_out(&mut out, entry);
    }
    if let Some(left) = page.left {
        msgpack::uint(&mut out, left);
    }
    out
}

pub fn send_request(submission: &Submission) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 5);
    msgpack::bin(&mut out, &submission.client_id);
    msgpack::bin(&mut out, &submission.destination);
    msgpack::bin(&mut out, &submission.title);
    msgpack::bin(&mut out, &submission.content);
    msgpack::array(&mut out, submission.fields.len());
    for (key, value) in &submission.fields {
        msgpack::array(&mut out, 2);
        msgpack::int(&mut out, *key);
        msgpack::bin(&mut out, value);
    }
    out
}

pub fn read_send_request(data: &[u8]) -> Result<Submission, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 5 {
        return Err(Malformed);
    }
    let client_id =
        <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let destination =
        <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let title = msgpack::read_bin(data, &mut p)?.to_vec();
    let content = msgpack::read_bin(data, &mut p)?.to_vec();
    let count = msgpack::array_len(data, &mut p)?;
    let mut fields = Vec::new();
    for _ in 0..count {
        if msgpack::array_len(data, &mut p)? != 2 {
            return Err(Malformed);
        }
        let key = msgpack::read_int(data, &mut p)?;
        let value = msgpack::read_bin(data, &mut p)?.to_vec();
        fields.push((key, value));
    }
    Ok(Submission {
        client_id,
        destination,
        title,
        content,
        fields,
    })
}

pub fn send_answer(id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::uint(&mut out, Status::Ok.code());
    msgpack::bin(&mut out, id);
    out
}

pub fn mark_request(mark: &Mark) -> Vec<u8> {
    let mut out = Vec::new();
    let seen_at = match mark.flag {
        MarkFlag::Seen { at } | MarkFlag::Arrived { at } => Some(at),
        MarkFlag::Read | MarkFlag::Deleted => None,
    };
    msgpack::array(&mut out, if seen_at.is_some() { 3 } else { 2 });
    msgpack::uint(&mut out, mark.flag.code());
    msgpack::array(&mut out, mark.ids.len());
    for id in &mark.ids {
        msgpack::bin(&mut out, id);
    }
    if let Some(at) = seen_at {
        msgpack::uint(&mut out, u64::from(at));
    }
    out
}

/// A mark, from a request or from the archive entry that keeps one. One
/// without ids, with more than [`MARK_IDS_MAX`], with a flag this loquax
/// does not know, or a seen mark without its time, is malformed.
pub fn read_mark(data: &[u8]) -> Result<Mark, Malformed> {
    let mut p = 0;
    let len = msgpack::array_len(data, &mut p)?;
    let code = msgpack::read_uint(data, &mut p)?;
    match (code, len) {
        (0 | 1, 2) | (SEEN_CODE | ARRIVED_CODE, 3) => {}
        _ => return Err(Malformed),
    }
    let count = msgpack::array_len(data, &mut p)?;
    if count == 0 || count > MARK_IDS_MAX {
        return Err(Malformed);
    }
    let mut ids = Vec::new();
    for _ in 0..count {
        ids.push(<[u8; 32]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?);
    }
    let flag = match code {
        0 => MarkFlag::Read,
        1 => MarkFlag::Deleted,
        _ => {
            let at = u32::try_from(msgpack::read_uint(data, &mut p)?).map_err(|_| Malformed)?;
            match code {
                SEEN_CODE => MarkFlag::Seen { at },
                _ => MarkFlag::Arrived { at },
            }
        }
    };
    Ok(Mark { flag, ids })
}

/// A [`super::KIND_PN_STATUS`] entry's bytes: the propagation node's word
/// per message id, `{ bin32 message id: str word, ... }`, in the shape of
/// the node's `/lev/status` answer but keyed by the message id every
/// device knows, not the `transient_id` only the sender's router saw.
pub fn pn_status_entry(
    answers: &[([u8; 32], leviculum_lxmf::lev_status::MessageStatus)],
) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::map(&mut out, answers.len());
    for (id, status) in answers {
        msgpack::bin(&mut out, id);
        msgpack::string(&mut out, status.word());
    }
    out
}

/// A [`super::KIND_PN_STATUS`] entry: none, or more than [`MARK_IDS_MAX`],
/// is malformed; a word this loquax does not know reads as `unknown`.
pub fn read_pn_status_entry(
    data: &[u8],
) -> Result<Vec<([u8; 32], leviculum_lxmf::lev_status::MessageStatus)>, Malformed> {
    match leviculum_lxmf::lev_status::decode_answer(data) {
        Ok(Ok(entries)) if !entries.is_empty() && entries.len() <= MARK_IDS_MAX => Ok(entries),
        _ => Err(Malformed),
    }
}

/// A name entry's bytes: the peer's address and the display name it
/// announced, not yet sanitised; every client sanitises it as it does an
/// announce it heard itself.
pub fn name_entry(peer: &[u8; 16], name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, peer);
    msgpack::string(&mut out, name);
    out
}

/// The bytes every name entry of `peer` begins with, whatever its name:
/// the store finds a peer's older entries by them.
pub fn name_entry_prefix(peer: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, peer);
    out
}

/// A hub entry's bytes (1599, [`super::KIND_HUB`]): the hub's `rrc.hub`
/// destination, then its name (empty for none), its hops (255 for
/// unknown), and when it was first and last heard, unix seconds. Its
/// first bytes are [`name_entry_prefix`]'s, so the store keeps a hub's
/// newest entry only, as it does a peer's name.
pub fn hub_entry(hub: &crate::ui::hubs::HeardHub) -> Vec<u8> {
    hub_entry_rooms(hub, None)
}

/// A hub entry ([`hub_entry`]) with the rooms the hub's answer to `/list`
/// named when the mailbox asked it (1643), as a fifth element after the
/// last hearing; none when it was not asked or did not answer. A client
/// from before it reads the four it knows and passes the fifth by.
pub fn hub_entry_rooms(hub: &crate::ui::hubs::HeardHub, rooms: Option<&[String]>) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, &hub.hash);
    msgpack::array(&mut out, 4 + usize::from(rooms.is_some()));
    msgpack::string(&mut out, hub.name.as_deref().unwrap_or_default());
    msgpack::uint(&mut out, u64::from(hub.hops.unwrap_or(u8::MAX)));
    msgpack::int(&mut out, hub.first);
    msgpack::int(&mut out, hub.last);
    if let Some(rooms) = rooms {
        msgpack::array(&mut out, rooms.len());
        for room in rooms {
            msgpack::string(&mut out, room);
        }
    }
    out
}

/// A hub entry ([`hub_entry`]); its name is cleaned again, as an announce's.
pub fn read_hub_entry(data: &[u8]) -> Result<crate::ui::hubs::HeardHub, Malformed> {
    read_hub_entry_rooms(data).map(|(hub, _)| hub)
}

/// A hub entry and the rooms it carries ([`hub_entry_rooms`]), `None` for
/// an entry without them.
pub fn read_hub_entry_rooms(
    data: &[u8],
) -> Result<(crate::ui::hubs::HeardHub, Option<Vec<String>>), Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let hash = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let len = msgpack::array_len(data, &mut p)?;
    if len < 4 {
        return Err(Malformed);
    }
    let name = crate::ui::hubs::shown_name(msgpack::read_str(data, &mut p)?);
    let hops = u8::try_from(msgpack::read_uint(data, &mut p)?)
        .ok()
        .filter(|h| *h != u8::MAX);
    let first = msgpack::read_int(data, &mut p)?;
    let last = msgpack::read_int(data, &mut p)?;
    let rooms = match len {
        4 => None,
        _ => {
            let count = msgpack::array_len(data, &mut p)?;
            let mut rooms = Vec::new();
            for _ in 0..count {
                rooms.push(String::from(msgpack::read_str(data, &mut p)?));
            }
            Some(rooms)
        }
    };
    Ok((
        crate::ui::hubs::HeardHub {
            hash,
            name,
            hops,
            first,
            last,
            answered: None,
        },
        rooms,
    ))
}

/// A name entry, which has a join request's shape: sixteen bytes and a
/// string.
pub fn read_name_entry(data: &[u8]) -> Result<([u8; 16], alloc::string::String), Malformed> {
    read_join_request(data)
}

/// A petname entry's bytes, which a `/petname` request carries as they
/// are: the peer's address and the reader's name for it, the empty string
/// for none. Shaped as a name entry ([`name_entry_prefix`] finds a peer's).
pub fn petname_entry(peer: &[u8; 16], petname: Option<&str>) -> Vec<u8> {
    name_entry(peer, petname.unwrap_or_default())
}

/// A petname entry or request: the peer, and its name, `None` for an empty
/// one. Sanitised by the client that applies it, as a petname from the
/// contacts file is.
pub fn read_petname_entry(
    data: &[u8],
) -> Result<([u8; 16], Option<alloc::string::String>), Malformed> {
    let (peer, name) = read_join_request(data)?;
    Ok((peer, Some(name).filter(|name| !name.is_empty())))
}

/// The reader's word on a peer and its conversation that every device of
/// the home shares (1146): blocked, pinned, archived, and muted until a
/// time. Sent whole each time one of them changes, so the newest entry of
/// a peer is all there is to know.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerState {
    pub blocked: bool,
    pub pinned: bool,
    pub archived: bool,
    /// Unix seconds the mute ends, `i64::MAX` for always; `None` unmuted.
    pub muted_until: Option<i64>,
}

/// A peer state entry's bytes, which a `/peerstate` request carries as
/// they are: the peer's address and its [`PeerState`], muted until 0 for
/// unmuted. Begins as a name entry ([`name_entry_prefix`] finds a peer's).
pub fn peer_state_entry(peer: &[u8; 16], state: &PeerState) -> Vec<u8> {
    let mut out = name_entry_prefix(peer);
    msgpack::array(&mut out, 4);
    msgpack::bool(&mut out, state.blocked);
    msgpack::bool(&mut out, state.pinned);
    msgpack::bool(&mut out, state.archived);
    msgpack::int(&mut out, state.muted_until.unwrap_or(0));
    out
}

/// A peer state entry or request. Later fields a newer client adds are
/// passed over.
pub fn read_peer_state_entry(data: &[u8]) -> Result<([u8; 16], PeerState), Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let peer = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    if msgpack::array_len(data, &mut p)? < 4 {
        return Err(Malformed);
    }
    let blocked = msgpack::read_bool(data, &mut p)?;
    let pinned = msgpack::read_bool(data, &mut p)?;
    let archived = msgpack::read_bool(data, &mut p)?;
    let muted_until = Some(msgpack::read_int(data, &mut p)?).filter(|&until| until > 0);
    Ok((
        peer,
        PeerState {
            blocked,
            pinned,
            archived,
            muted_until,
        },
    ))
}

/// A `/key` request: a contact's address and its identity's public key.
pub fn key_request(peer: &[u8; 16], public_key: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, peer);
    msgpack::bin(&mut out, public_key);
    out
}

/// A `/key` request's address and key. Whether they belong together is
/// the home node's check ([`crate::home::lxmf_address`]), not the wire's.
pub fn read_key_request(data: &[u8]) -> Result<([u8; 16], [u8; 64]), Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let peer = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let key = <[u8; 64]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    Ok((peer, key))
}

/// The most members one `/group` names (1307): a family, a team, a
/// club's board; the home sends each an invitation at once.
pub const GROUP_MEMBERS_MAX: usize = 64;

/// One `/group` (1307, `docs/drafts/group-chats-study-2026-11.md` 4.1): make
/// a group hosted by the home, its admin the home's owner, and invite the
/// members, each under the name the admin gives it. The same `client_id`
/// handed in again is the same group. The invitations are English on
/// the wire whatever the admin's language (1348), so the request names
/// none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRequest {
    pub client_id: [u8; 16],
    pub name: alloc::string::String,
    pub members: Vec<([u8; 16], alloc::string::String)>,
}

pub fn group_request(request: &GroupRequest) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 3);
    msgpack::bin(&mut out, &request.client_id);
    msgpack::string(&mut out, &request.name);
    members_out(&mut out, &request.members);
    out
}

/// The members of a `/group` or a `/groupadd`, each under its name.
fn members_out(out: &mut Vec<u8>, members: &[([u8; 16], alloc::string::String)]) {
    msgpack::array(out, members.len());
    for (address, name) in members {
        msgpack::array(out, 2);
        msgpack::bin(out, address);
        msgpack::string(out, name);
    }
}

/// A phone of 1307 pass six sends a `/group` or a `/groupadd` its
/// language as a fourth element, a string; the home reads it past and
/// takes nothing from it (1348: the invitations are English on the wire),
/// so such a phone still makes its groups.
fn legacy_lang(data: &[u8], p: &mut usize, len: usize) -> Result<(), Malformed> {
    if len == 4 {
        msgpack::read_str(data, p)?;
    }
    Ok(())
}

/// A `/group` as the home reads it: a group with no name, no member or more
/// than [`GROUP_MEMBERS_MAX`] is malformed.
pub fn read_group_request(data: &[u8]) -> Result<GroupRequest, Malformed> {
    let mut p = 0;
    let len = msgpack::array_len(data, &mut p)?;
    if !(3..=4).contains(&len) {
        return Err(Malformed);
    }
    let client_id =
        <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let name: alloc::string::String = msgpack::read_str(data, &mut p)?.trim().into();
    let count = msgpack::array_len(data, &mut p)?;
    if name.is_empty() || count == 0 || count > GROUP_MEMBERS_MAX {
        return Err(Malformed);
    }
    let mut members = Vec::with_capacity(count);
    for _ in 0..count {
        if msgpack::array_len(data, &mut p)? != 2 {
            return Err(Malformed);
        }
        let address =
            <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
        let name = msgpack::read_str(data, &mut p)?.trim().into();
        members.push((address, name));
    }
    legacy_lang(data, &mut p, len)?;
    Ok(GroupRequest {
        client_id,
        name,
        members,
    })
}

/// One `/groupadd` (1307 G2, study 4.5): the admin's home adds `members`
/// to its group `group`, each invited under the name the admin gives it.
/// A member already in it is skipped, so the same request again adds
/// nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupAddRequest {
    pub client_id: [u8; 16],
    pub group: [u8; 16],
    pub members: Vec<([u8; 16], alloc::string::String)>,
}

pub fn group_add_request(request: &GroupAddRequest) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 3);
    msgpack::bin(&mut out, &request.client_id);
    msgpack::bin(&mut out, &request.group);
    members_out(&mut out, &request.members);
    out
}

/// A `/groupadd` as the home reads it: no member, or more than
/// [`GROUP_MEMBERS_MAX`], is malformed.
pub fn read_group_add_request(data: &[u8]) -> Result<GroupAddRequest, Malformed> {
    let mut p = 0;
    let len = msgpack::array_len(data, &mut p)?;
    if !(3..=4).contains(&len) {
        return Err(Malformed);
    }
    let client_id =
        <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let group = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let count = msgpack::array_len(data, &mut p)?;
    if count == 0 || count > GROUP_MEMBERS_MAX {
        return Err(Malformed);
    }
    let mut members = Vec::with_capacity(count);
    for _ in 0..count {
        if msgpack::array_len(data, &mut p)? != 2 {
            return Err(Malformed);
        }
        let address =
            <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
        let name = msgpack::read_str(data, &mut p)?.trim().into();
        members.push((address, name));
    }
    legacy_lang(data, &mut p, len)?;
    Ok(GroupAddRequest {
        client_id,
        group,
        members,
    })
}

/// One `/groupdelete` (1307 pass four): the admin's home deletes its group
/// `group` for everyone. Answered with a status; asked again, or for a
/// group no longer hosted, it is `Ok`: the group is gone either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupDeleteRequest {
    pub client_id: [u8; 16],
    pub group: [u8; 16],
}

pub fn group_delete_request(request: &GroupDeleteRequest) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, &request.client_id);
    msgpack::bin(&mut out, &request.group);
    out
}

/// A `/groupdelete` as the home reads it.
pub fn read_group_delete_request(data: &[u8]) -> Result<GroupDeleteRequest, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let client_id =
        <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let group = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    Ok(GroupDeleteRequest { client_id, group })
}

pub fn group_answer(group: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::uint(&mut out, Status::Ok.code());
    msgpack::bin(&mut out, group);
    out
}

/// A `/group` answer: the group's address, or the status that refused it.
pub fn read_group_answer(data: &[u8]) -> Result<Result<[u8; 16], Status>, Malformed> {
    let status = read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let group = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    Ok(Ok(group))
}

/// A `/send` answer: the message id, or the status that refused it.
pub fn read_send_answer(data: &[u8]) -> Result<Result<[u8; 32], Status>, Malformed> {
    let status = read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let id = <[u8; 32]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    Ok(Ok(id))
}

/// A `/sync` answer: the page, or the status that refused it.
pub fn read_sync_answer(data: &[u8]) -> Result<Result<Page, Status>, Malformed> {
    let status = read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    let len = msgpack::array_len(data, &mut p)?;
    if !(4..=5).contains(&len) {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let first = msgpack::read_uint(data, &mut p)?;
    let more = msgpack::read_bool(data, &mut p)?;
    let count = msgpack::array_len(data, &mut p)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        if msgpack::array_len(data, &mut p)? != 4 {
            return Err(Malformed);
        }
        let seq = msgpack::read_uint(data, &mut p)?;
        let kind = u8::try_from(msgpack::read_uint(data, &mut p)?).map_err(|_| Malformed)?;
        let at = msgpack::read_int(data, &mut p)?;
        let packed = msgpack::read_bin(data, &mut p)?.to_vec();
        entries.push(Entry {
            seq,
            kind,
            at,
            packed,
        });
    }
    let left = match len {
        5 => Some(msgpack::read_uint(data, &mut p)?),
        _ => None,
    };
    Ok(Ok(Page {
        entries,
        more,
        first,
        left,
    }))
}

/// A `/move` or `/moved` request (1281): the move token the old device's
/// owner opened, and the archive sequence the new device holds up to.
pub fn move_request(token: &[u8; 16], cursor: u64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, token);
    msgpack::uint(&mut out, cursor);
    out
}

pub fn read_move_request(data: &[u8]) -> Result<([u8; 16], u64), Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let token = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    Ok((token, msgpack::read_uint(data, &mut p)?))
}

/// One archive entry as a move carries it: with its id, which the new
/// mailbox keeps so a message that comes again is still archived once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moving {
    pub seq: u64,
    pub kind: u8,
    pub id: [u8; 32],
    pub at: i64,
    pub packed: Vec<u8>,
}

/// One `/move` page (1281): the identity's private key, the delivery
/// destination's own ratchets as the node keeps them (signed, empty when
/// it has none), the identities its node knows as it last kept them (the
/// contacts' keys; empty when none were kept), the name the mailbox
/// announces (empty for none), and the archive entries after the
/// request's cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MovePage {
    pub private_key: Vec<u8>,
    pub ratchets: Vec<u8>,
    pub known: Vec<u8>,
    pub name: alloc::string::String,
    pub first: u64,
    pub more: bool,
    pub entries: Vec<Moving>,
}

pub fn move_answer(page: &MovePage) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 8);
    msgpack::uint(&mut out, Status::Ok.code());
    msgpack::bin(&mut out, &page.private_key);
    msgpack::bin(&mut out, &page.ratchets);
    msgpack::bin(&mut out, &page.known);
    msgpack::string(&mut out, &page.name);
    msgpack::uint(&mut out, page.first);
    msgpack::bool(&mut out, page.more);
    msgpack::array(&mut out, page.entries.len());
    for entry in &page.entries {
        msgpack::array(&mut out, 5);
        msgpack::uint(&mut out, entry.seq);
        msgpack::uint(&mut out, u64::from(entry.kind));
        msgpack::bin(&mut out, &entry.id);
        msgpack::int(&mut out, entry.at);
        msgpack::bin(&mut out, &entry.packed);
    }
    out
}

/// A `/move` answer: the page, or the status that refused it.
pub fn read_move_answer(data: &[u8]) -> Result<Result<MovePage, Status>, Malformed> {
    let status = read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 8 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let private_key = msgpack::read_bin(data, &mut p)?.to_vec();
    let ratchets = msgpack::read_bin(data, &mut p)?.to_vec();
    let known = msgpack::read_bin(data, &mut p)?.to_vec();
    let name = alloc::string::String::from(msgpack::read_str(data, &mut p)?);
    let first = msgpack::read_uint(data, &mut p)?;
    let more = msgpack::read_bool(data, &mut p)?;
    let count = msgpack::array_len(data, &mut p)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        if msgpack::array_len(data, &mut p)? != 5 {
            return Err(Malformed);
        }
        let seq = msgpack::read_uint(data, &mut p)?;
        let kind = u8::try_from(msgpack::read_uint(data, &mut p)?).map_err(|_| Malformed)?;
        let id = <[u8; 32]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
        let at = msgpack::read_int(data, &mut p)?;
        let packed = msgpack::read_bin(data, &mut p)?.to_vec();
        entries.push(Moving {
            seq,
            kind,
            id,
            at,
            packed,
        });
    }
    Ok(Ok(MovePage {
        private_key,
        ratchets,
        known,
        name,
        first,
        more,
        entries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_join_request_round_trips() {
        let data = join_request(&[7; 16], "laptop");
        assert_eq!(read_join_request(&data), Ok(([7; 16], "laptop".into())));
    }

    #[test]
    fn a_name_entry_round_trips_and_a_mark_is_none() {
        let data = name_entry(&[5; 16], "Reviewer Berlin");
        assert_eq!(
            read_name_entry(&data),
            Ok(([5; 16], "Reviewer Berlin".into()))
        );
        let mark = Mark {
            flag: MarkFlag::Read,
            ids: alloc::vec![[1; 32]],
        };
        assert_eq!(read_name_entry(&mark_request(&mark)), Err(Malformed));
    }

    /// 1146: a peer state round trips, unmuted as 0, and it is no name.
    #[test]
    fn a_peer_state_entry_round_trips() {
        for state in [
            PeerState::default(),
            PeerState {
                blocked: true,
                pinned: false,
                archived: true,
                muted_until: Some(i64::MAX),
            },
            PeerState {
                pinned: true,
                muted_until: Some(1_800_000_000),
                ..PeerState::default()
            },
        ] {
            let data = peer_state_entry(&[9; 16], &state);
            assert!(data.starts_with(&name_entry_prefix(&[9; 16])));
            assert_eq!(read_peer_state_entry(&data), Ok(([9; 16], state)));
            assert_eq!(read_name_entry(&data), Err(Malformed));
        }
        assert_eq!(
            read_peer_state_entry(&name_entry(&[9; 16], "x")),
            Err(Malformed)
        );
    }

    #[test]
    fn a_page_round_trips() {
        let page = Page {
            entries: alloc::vec![
                Entry {
                    seq: 3,
                    kind: 0,
                    at: 1_800_000_000,
                    packed: alloc::vec![1, 2, 3],
                },
                Entry {
                    seq: 70_000,
                    kind: 0,
                    at: -1,
                    packed: alloc::vec![0; 300],
                },
            ],
            more: true,
            first: 2,
            left: None,
        };
        assert_eq!(read_sync_answer(&sync_answer(&page)), Ok(Ok(page.clone())));
        // A paged answer says how many wait (1415); a home that says none
        // is read as before.
        let counted = Page {
            left: Some(41),
            ..page.clone()
        };
        let answer = sync_answer(&counted);
        assert_eq!(answer.len(), sync_answer(&page).len() + 1);
        assert_eq!(read_sync_answer(&answer), Ok(Ok(counted)));
        // The placeholder the cut counts with is the longest a count gets.
        let placeholder = Page {
            left: Some(LEFT_UNCOUNTED),
            ..page.clone()
        };
        assert_eq!(
            sync_answer(&placeholder).len(),
            sync_answer(&page).len() + 9
        );
    }

    #[test]
    fn a_refusal_decodes_as_its_status() {
        let data = status_answer(Status::NotAdmitted);
        assert_eq!(read_sync_answer(&data), Ok(Err(Status::NotAdmitted)));
        assert_eq!(read_status(&data), Ok(Status::NotAdmitted));
    }

    #[test]
    fn a_submission_and_its_answer_round_trip() {
        let submission = Submission {
            client_id: [3; 16],
            destination: [4; 16],
            title: b"t".to_vec(),
            content: "grüß dich".as_bytes().to_vec(),
            fields: alloc::vec![(2, alloc::vec![0xc3]), (-1, alloc::vec![0x01])],
        };
        assert_eq!(
            read_send_request(&send_request(&submission)),
            Ok(submission)
        );
        assert_eq!(read_send_answer(&send_answer(&[8; 32])), Ok(Ok([8; 32])));
        assert_eq!(
            read_send_answer(&status_answer(Status::NotAdmitted)),
            Ok(Err(Status::NotAdmitted))
        );
    }

    /// A three-element request as a 1307 pass six phone sent it: its
    /// language appended as a fourth element.
    fn legacy(request: &[u8], lang: &str) -> Vec<u8> {
        assert_eq!(request[0], 0x93, "a three-element array");
        let mut out = alloc::vec![0x94];
        out.extend_from_slice(&request[1..]);
        msgpack::string(&mut out, lang);
        out
    }

    /// 1307: a `/group` and its answer round trip; a group without a name
    /// or a member, or with one member too many, is malformed.
    #[test]
    fn a_group_request_and_its_answer_round_trip() {
        let request = GroupRequest {
            client_id: [5; 16],
            name: "Familie".into(),
            members: alloc::vec![([1; 16], "Anna".into()), ([2; 16], "Bob".into())],
        };
        assert_eq!(
            read_group_request(&group_request(&request)),
            Ok(request.clone())
        );
        // 1348: three elements, the language gone; a 1307 pass six phone's
        // fourth, its language, is read past and changes nothing.
        let mut p = 0;
        assert_eq!(msgpack::array_len(&group_request(&request), &mut p), Ok(3));
        assert_eq!(
            read_group_request(&legacy(&group_request(&request), "de")),
            Ok(request.clone())
        );
        let mut garbled = legacy(&group_request(&request), "de");
        let at = garbled.len() - 3;
        garbled[at] = 0xc4; // a bin where the language's string was
        assert_eq!(
            read_group_request(&garbled),
            Err(Malformed),
            "control: a fourth element that is no string is refused"
        );
        assert_eq!(read_group_answer(&group_answer(&[9; 16])), Ok(Ok([9; 16])));
        assert_eq!(
            read_group_answer(&status_answer(Status::NotAdmitted)),
            Ok(Err(Status::NotAdmitted))
        );
        let nameless = GroupRequest {
            name: "  ".into(),
            ..request.clone()
        };
        assert_eq!(
            read_group_request(&group_request(&nameless)),
            Err(Malformed)
        );
        let alone = GroupRequest {
            members: Vec::new(),
            ..request.clone()
        };
        assert_eq!(read_group_request(&group_request(&alone)), Err(Malformed));
        let crowd = GroupRequest {
            members: (0..=GROUP_MEMBERS_MAX)
                .map(|n| ([n as u8; 16], "x".into()))
                .collect(),
            ..request.clone()
        };
        assert_eq!(read_group_request(&group_request(&crowd)), Err(Malformed));
        let full = GroupRequest {
            members: crowd.members[..GROUP_MEMBERS_MAX].to_vec(),
            ..request
        };
        assert_eq!(read_group_request(&group_request(&full)), Ok(full));
        // `/groupadd` (G2): round trips; none or too many is malformed.
        let add = GroupAddRequest {
            client_id: [3; 16],
            group: [4; 16],
            members: vec![([5; 16], "Cara".into())],
        };
        assert_eq!(
            read_group_add_request(&group_add_request(&add)),
            Ok(add.clone())
        );
        assert_eq!(
            read_group_add_request(&legacy(&group_add_request(&add), "de")),
            Ok(add.clone())
        );
        let none = GroupAddRequest {
            members: Vec::new(),
            ..add.clone()
        };
        assert_eq!(
            read_group_add_request(&group_add_request(&none)),
            Err(Malformed)
        );
        let crowd = GroupAddRequest {
            members: (0..=GROUP_MEMBERS_MAX)
                .map(|i| ([i as u8; 16], "x".into()))
                .collect(),
            ..add
        };
        assert_eq!(
            read_group_add_request(&group_add_request(&crowd)),
            Err(Malformed)
        );
        // `/groupdelete` (pass four): round trips; a group of 8 bytes is
        // malformed.
        let delete = GroupDeleteRequest {
            client_id: [6; 16],
            group: [7; 16],
        };
        assert_eq!(
            read_group_delete_request(&group_delete_request(&delete)),
            Ok(delete)
        );
        let mut short = Vec::new();
        msgpack::array(&mut short, 2);
        msgpack::bin(&mut short, &[6; 16]);
        msgpack::bin(&mut short, &[7; 8]);
        assert_eq!(read_group_delete_request(&short), Err(Malformed));
    }

    #[test]
    fn a_mark_round_trips_and_an_empty_or_unknown_one_is_malformed() {
        let mark = Mark {
            flag: MarkFlag::Deleted,
            ids: alloc::vec![[1; 32], [2; 32]],
        };
        assert_eq!(read_mark(&mark_request(&mark)), Ok(mark));
        let empty = Mark {
            flag: MarkFlag::Read,
            ids: Vec::new(),
        };
        assert_eq!(read_mark(&mark_request(&empty)), Err(Malformed));
        let mut unknown = Vec::new();
        msgpack::array(&mut unknown, 2);
        msgpack::uint(&mut unknown, 7);
        msgpack::array(&mut unknown, 1);
        msgpack::bin(&mut unknown, &[1; 32]);
        assert_eq!(read_mark(&unknown), Err(Malformed));
    }

    /// 1409: the home's word of what the node answered is the node's
    /// answer keyed by message id; none, too many or no map is malformed.
    #[test]
    fn a_pn_status_entry_round_trips_and_is_the_answers_shape() {
        use leviculum_lxmf::lev_status::MessageStatus;
        let answers = [
            ([0x61; 32], MessageStatus::Collected),
            ([0x62; 32], MessageStatus::Expired),
        ];
        let data = pn_status_entry(&answers);
        assert_eq!(&data[..3], &[0x82, 0xc4, 0x20]);
        assert_eq!(&data[35..45], b"\xa9collected");
        assert_eq!(read_pn_status_entry(&data), Ok(answers.to_vec()));
        assert_eq!(read_pn_status_entry(&pn_status_entry(&[])), Err(Malformed));
        let many: Vec<_> = (0..=MARK_IDS_MAX)
            .map(|i| ([i as u8; 32], MessageStatus::Collected))
            .collect();
        assert_eq!(
            read_pn_status_entry(&pn_status_entry(&many)),
            Err(Malformed)
        );
        assert_eq!(read_pn_status_entry(&[0x90]), Err(Malformed));
    }

    /// 1234: a seen mark carries the read time, begins with the prefix the
    /// store looks for, and without its time it is malformed.
    #[test]
    fn a_seen_mark_round_trips_with_its_time() {
        let seen = Mark {
            flag: MarkFlag::Seen { at: 1_800_000_000 },
            ids: alloc::vec![[1; 32], [2; 32]],
        };
        let data = mark_request(&seen);
        assert!(data.starts_with(&SEEN_MARK_PREFIX));
        assert_eq!(read_mark(&data), Ok(seen));
        let read = mark_request(&Mark {
            flag: MarkFlag::Read,
            ids: alloc::vec![[1; 32]],
        });
        assert!(!read.starts_with(&SEEN_MARK_PREFIX));
        let mut timeless = Vec::new();
        msgpack::array(&mut timeless, 2);
        msgpack::uint(&mut timeless, SEEN_CODE);
        msgpack::array(&mut timeless, 1);
        msgpack::bin(&mut timeless, &[1; 32]);
        assert_eq!(read_mark(&timeless), Err(Malformed));
        let mut read_with_time = Vec::new();
        msgpack::array(&mut read_with_time, 3);
        msgpack::uint(&mut read_with_time, 0);
        msgpack::array(&mut read_with_time, 1);
        msgpack::bin(&mut read_with_time, &[1; 32]);
        msgpack::uint(&mut read_with_time, 5);
        assert_eq!(read_mark(&read_with_time), Err(Malformed));
    }

    /// 1462: an arrived mark carries the device's time, begins with its
    /// own prefix, is no seen mark, and without its time it is malformed.
    #[test]
    fn an_arrived_mark_round_trips_with_its_time() {
        let arrived = Mark {
            flag: MarkFlag::Arrived { at: 1_800_000_000 },
            ids: alloc::vec![[1; 32], [2; 32]],
        };
        let data = mark_request(&arrived);
        assert!(data.starts_with(&ARRIVED_MARK_PREFIX));
        assert!(!data.starts_with(&SEEN_MARK_PREFIX));
        assert_eq!(read_mark(&data), Ok(arrived));
        let mut timeless = Vec::new();
        msgpack::array(&mut timeless, 2);
        msgpack::uint(&mut timeless, ARRIVED_CODE);
        msgpack::array(&mut timeless, 1);
        msgpack::bin(&mut timeless, &[1; 32]);
        assert_eq!(read_mark(&timeless), Err(Malformed));
        let mut unknown = Vec::new();
        msgpack::array(&mut unknown, 3);
        msgpack::uint(&mut unknown, ARRIVED_CODE + 1);
        msgpack::array(&mut unknown, 1);
        msgpack::bin(&mut unknown, &[1; 32]);
        msgpack::uint(&mut unknown, 5);
        assert_eq!(read_mark(&unknown), Err(Malformed));
    }

    #[test]
    fn a_watch_and_a_wake_round_trip_and_a_stranger_is_no_wake() {
        assert_eq!(read_watch_request(&watch_request(9)), Ok(9));
        assert_eq!(read_watch_answer(&watch_answer(41)), Ok(Ok(41)));
        assert_eq!(
            read_watch_answer(&status_answer(Status::NotAdmitted)),
            Ok(Err(Status::NotAdmitted))
        );
        let data = wake(u64::MAX);
        assert_eq!(read_wake(&data), Ok(u64::MAX));
        // A handful of bytes: one link packet on any medium.
        assert!(data.len() <= 16, "{}", data.len());
        let mut other = Vec::new();
        msgpack::array(&mut other, 2);
        msgpack::string(&mut other, "push");
        msgpack::uint(&mut other, 1);
        assert_eq!(read_wake(&other), Err(Malformed));
        assert_eq!(read_wake(&sync_request(1)), Err(Malformed));
    }

    #[test]
    fn a_cursor_round_trips() {
        assert_eq!(read_sync_request(&sync_request(u64::MAX)), Ok(u64::MAX));
    }

    #[test]
    fn a_move_page_round_trips_and_a_refusal_is_its_status() {
        assert_eq!(
            read_move_request(&move_request(&[3; 16], 77)),
            Ok(([3; 16], 77))
        );
        assert_eq!(read_move_request(&sync_request(1)), Err(Malformed));
        let page = MovePage {
            private_key: vec![9; 64],
            ratchets: Vec::new(),
            known: vec![1, 2],
            name: "Anna".into(),
            first: 2,
            more: true,
            entries: vec![Moving {
                seq: 5,
                kind: crate::home::KIND_NAME,
                id: [7; 32],
                at: -1,
                packed: vec![0, 1, 2],
            }],
        };
        assert_eq!(read_move_answer(&move_answer(&page)), Ok(Ok(page)));
        assert_eq!(
            read_move_answer(&status_answer(Status::Behind)),
            Ok(Err(Status::Behind))
        );
        assert_eq!(
            read_status(&status_answer(Status::Behind)),
            Ok(Status::Behind)
        );
    }
}
