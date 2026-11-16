//! The bench's arrivals (orders 1540, 1562): a message that comes in with
//! no network and no mailbox, so a walk can open an unread chat as often
//! as it likes, which is Lew's case of 2026-11-07 (an unread chat, Enter,
//! the read mark and the rows written while the UI core folds).
//!
//! Every arrival comes from one synthetic peer, [`BENCH_PEER`], into one
//! conversation the list names "Bench", never into a real chat (1562:
//! 1540 folded them into the row's chat, and Lew's chats filled with
//! `bench arrival <n>`). Each carries the bench's flag, [`SIGNATURE`] in
//! its signature's place, which no message a sender signed can carry.
//! A release image drops flagged entries at restore ([`Arrivals::Dropped`])
//! and so never lists "Bench"; the bench image's `bench forget` takes them
//! out of the ring ([`Forgetting`]), with the unflagged arrivals 1540 left
//! in real chats. Only the M9's bench image makes arrivals
//! (`TYPE_BENCH_ARRIVE`, `leviculum-esp`'s `cfg(m9_bench)`); the host tests
//! pin all of it.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use leviculum_lxmf::msgpack;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire::{self, Entry};
use loquax::home::{KIND_MARK, KIND_NAME, KIND_RECEIVED};
use loquax::ui::{Pane, UiModel};

use crate::field::Field;

/// The bench's peer: the source of every arrival. A real peer's address
/// is the first sixteen bytes of a SHA-256 over its identity's keys, so
/// no identity can be made to have these sixteen ASCII bytes.
pub const BENCH_PEER: [u8; 16] = *b"loquax-m9 bench!";

/// The name the list shows for [`BENCH_PEER`].
pub const BENCH_NAME: &str = "Bench";

/// The bench's flag: what an arrival carries where a sender's Ed25519
/// signature goes. Not a signature (the board never checks one,
/// `loquax::home::board::fold`), and no signature a key makes is these
/// bytes.
pub const SIGNATURE: [u8; 64] =
    *b"loquax-m9 bench arrival: no sender signed it, no home has it....";

/// The words every arrival's text starts with, 1540's and 1562's.
pub const WORDS: &str = "bench arrival ";

/// The time an arrival is written at when no chat says one: 2026-11-01.
const NO_TIME: i64 = 1_793_500_000;

/// Where a packed LXMF message keeps its parts: destination, source,
/// signature, then the payload.
const SOURCE: core::ops::Range<usize> = 16..32;
const SIGNED: core::ops::Range<usize> = 32..96;

/// What a restore does with the bench's entries (1562).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrivals {
    /// The bench image: "Bench" is listed and opens.
    Shown,
    /// A release image: flagged entries are not folded, and a summary's
    /// "Bench" row is not listed.
    Dropped,
}

/// An arrival: the time it is written at, a second after the newest
/// chat's, so "Bench" goes to the list's top; and whether the model
/// lists "Bench" already (when not, the arrival brings its name along).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    pub at: i64,
    pub named: bool,
}

/// The next arrival into `model`.
pub fn arrival(model: &UiModel<Field>) -> Arrival {
    let at = model
        .conversations
        .iter()
        .filter_map(|c| c.last_written())
        .max()
        .unwrap_or(NO_TIME)
        + 1;
    let named = model.conversations.iter().any(|c| c.peer == BENCH_PEER);
    Arrival { at, named }
}

/// The index of "Bench" among `model`'s conversations, when it is there.
pub fn index(model: &UiModel<Field>) -> Option<usize> {
    model
        .conversations
        .iter()
        .position(|c| c.peer == BENCH_PEER)
}

/// Put the list's cursor on "Bench" while the list shows: the walk's
/// Enter opens it. Whether it is there.
pub fn select(model: &mut UiModel<Field>) -> bool {
    let Some(index) = index(model) else {
        return false;
    };
    if model.focus == Pane::Conversations {
        model.selected = index;
    }
    true
}

/// The archive entries of the `n`th arrival since boot to the home at
/// `address` (its words say which, so every arrival is its own message):
/// the name "Bench" when the model lacks it, then the message, received
/// from [`BENCH_PEER`] as a home keeps one, in LXMF's packed form, the
/// flag in its signature's place. Every `seq` is the store's cursor: the
/// entries move nothing the home's sequences say.
pub fn entries(address: [u8; 16], arrival: Arrival, n: u32, seq: u64) -> Vec<Entry> {
    let mut out = Vec::with_capacity(2);
    if !arrival.named {
        out.push(Entry {
            seq,
            kind: KIND_NAME,
            at: arrival.at,
            packed: wire::name_entry(&BENCH_PEER, BENCH_NAME),
        });
    }
    let words = alloc::format!("{WORDS}{n}");
    let mut packed = Vec::with_capacity(160);
    packed.extend_from_slice(&address);
    packed.extend_from_slice(&BENCH_PEER);
    packed.extend_from_slice(&SIGNATURE);
    msgpack::array(&mut packed, 4);
    msgpack::f64(&mut packed, arrival.at as f64);
    msgpack::bin(&mut packed, b"");
    msgpack::bin(&mut packed, words.as_bytes());
    msgpack::map(&mut packed, 0);
    out.push(Entry {
        seq,
        kind: KIND_RECEIVED,
        at: arrival.at,
        packed,
    });
    out
}

/// Whether `entry` is the bench's own (1562): a message with the flag or
/// from [`BENCH_PEER`], or the name of [`BENCH_PEER`]. What a release
/// image does not fold.
pub fn is_bench(entry: &Entry) -> bool {
    match entry.kind {
        KIND_RECEIVED => {
            entry.packed.get(SIGNED) == Some(&SIGNATURE[..])
                || entry.packed.get(SOURCE) == Some(&BENCH_PEER[..])
        }
        KIND_NAME => wire::read_name_entry(&entry.packed).is_ok_and(|(peer, _)| peer == BENCH_PEER),
        _ => false,
    }
}

/// Whether `entry` is an arrival order 1540 made (before the flag): a
/// received message whose signature is sixty-four zero bytes and whose
/// text starts with [`WORDS`]. A sender's signature is never all zero,
/// so a real message is never taken for one.
pub fn is_unflagged(entry: &Entry) -> bool {
    if entry.kind != KIND_RECEIVED
        || entry
            .packed
            .get(SIGNED)
            .is_none_or(|sig| sig.iter().any(|&b| b != 0))
    {
        return false;
    }
    Message::unpack(&entry.packed, None, None, DeliveryMethod::Direct)
        .is_ok_and(|m| m.content.starts_with(WORDS.as_bytes()))
}

/// What `bench forget` takes out of the ring (1562), entry by entry,
/// oldest first ([`loquax::home::board::store::Store::forget`]): the
/// bench's own entries, 1540's unflagged arrivals, and a read mark
/// whose every id is one of theirs (the board marked an arrival read
/// when the walk opened it). A mark that names a real message too is
/// kept: it is the reader's.
#[derive(Debug, Default)]
pub struct Forgetting {
    /// The ids of the messages taken out so far.
    ids: BTreeSet<[u8; 32]>,
    /// Entries taken out, by what they were.
    pub flagged: u64,
    pub unflagged: u64,
    pub marks: u64,
}

impl Forgetting {
    /// Whether `entry` goes.
    pub fn drops(&mut self, entry: &Entry) -> bool {
        if entry.kind == KIND_MARK {
            let gone = wire::read_mark(&entry.packed).is_ok_and(|mark| {
                !mark.ids.is_empty() && mark.ids.iter().all(|id| self.ids.contains(id))
            });
            self.marks += u64::from(gone);
            return gone;
        }
        let (bench, unflagged) = (is_bench(entry), is_unflagged(entry));
        if !bench && !unflagged {
            return false;
        }
        if entry.kind == KIND_RECEIVED {
            if let Ok(message) = Message::unpack(&entry.packed, None, None, DeliveryMethod::Direct)
            {
                self.ids.insert(message.message_id);
            }
        }
        match bench {
            true => self.flagged += 1,
            false => self.unflagged += 1,
        }
        true
    }
}

/// The ids of "Bench"'s messages in `model`: the marks the board sends
/// its home leave them out (1562), the home never had them.
pub fn ids(model: &UiModel<Field>) -> BTreeSet<[u8; 32]> {
    model
        .conversations
        .iter()
        .filter(|c| c.peer == BENCH_PEER)
        .flat_map(|c| c.messages.iter().filter_map(|m| m.id))
        .collect()
}
