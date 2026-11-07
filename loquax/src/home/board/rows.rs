//! The board's boot from summaries (1521): a log of one record per
//! conversation beside the message store, so a boot lists the
//! conversations from 300 records instead of folding 4067 entries, and a
//! conversation's messages are read from the ring when it opens, through
//! an index of the ring sectors its entries lie in.
//!
//! The `messages` partition is split ([`split`]): the ring of
//! [`super::store`] in the first sectors, the summary log in the last
//! eighth. The summary log is a [`Store`] of its own (the same frames,
//! the same ring, under the home's tag with its last byte turned,
//! [`rows_tag`]), whose entries are records by their kind:
//!
//! ```text
//! kind  seq          packed
//!    1  0            ROW: the peer (16), u32 n, the conversation's Row
//!                    (n bytes, `ui::listing::Row`, none while the
//!                    model has said none), its index: u32 entries,
//!                    u32 m, m sector sequences (u32 each)
//!    2  0            GONE: the peer (16), its conversation deleted
//!    3  0            NAME: the peer (16), the name it announced (UTF-8)
//!    4  0            STATE: the ring's end (u32 sequence, u32 offset),
//!                    cursor (u64), entries (u64), u32 m and m sectors
//!                    holding marks, u32 k and k asks (id 16, tag 1,
//!                    u32 n, n request bytes)
//!    5  sequence     SNAP: a snapshot begins in the log's sector of
//!                    this sequence
//! ```
//!
//! Records are replayed oldest first, the newest of a peer winning. A
//! record is written whenever its conversation changes (one flash write
//! per sector a turn's records touch). Before the log's ring comes round
//! to a sector, a snapshot writes every live record again
//! ([`RowLog::snapshot_due`]: every half of the log), so the newest
//! record of every conversation is always within the sectors the ring
//! still holds.
//!
//! The STATE record says where the message ring ended when it was
//! written. The boot trusts the records only when the ring ends there
//! still ([`super::store::Store::resume`]); a ring that went on (a power
//! cut between a page's write and its records) or a store from before
//! the records (no STATE) is read whole once, the old cost, and the
//! records are written anew from it.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::store::{Appended, End, PendingAsk, Sectors, Span, Store};
use crate::home::wire::Entry;
use crate::home::{KIND_MARK, KIND_NAME, KIND_RECEIVED, KIND_SENT};
use crate::ui::listing::Row;
use crate::ui::{update, Message, UiEvent, UiModel};
use leviculum_ui::input::InputBuffer;

/// A record's kind in the summary log.
pub const ROW: u8 = 1;
pub const GONE: u8 = 2;
pub const NAME: u8 = 3;
pub const STATE: u8 = 4;
pub const SNAP: u8 = 5;

/// Sectors of `dev` from `first` on, `count` of them, as a [`Sectors`]
/// of their own.
pub struct Window<'a, S> {
    dev: &'a mut S,
    first: u32,
    count: u32,
}

impl<'a, S: Sectors> Window<'a, S> {
    pub fn new(dev: &'a mut S, first: u32, count: u32) -> Self {
        Self { dev, first, count }
    }
}

impl<S: Sectors> Sectors for Window<'_, S> {
    type Error = S::Error;

    fn count(&self) -> u32 {
        self.count
    }

    // The store asks only below `count`; the window adds its start.
    fn read(&mut self, sector: u32, offset: usize, buf: &mut [u8]) -> Result<(), S::Error> {
        self.dev.read(self.first + sector, offset, buf)
    }

    fn write(&mut self, sector: u32, offset: usize, data: &[u8]) -> Result<(), S::Error> {
        self.dev.write(self.first + sector, offset, data)
    }

    fn erase(&mut self, sector: u32) -> Result<(), S::Error> {
        self.dev.erase(self.first + sector)
    }
}

/// How a `messages` partition of `count` sectors is split: the ring's
/// sectors, then the summary log's, the last eighth. `None` for one too
/// small to split, which keeps the whole ring and boots by folding.
pub fn split(count: u32) -> Option<(u32, u32)> {
    if count < 64 {
        return None;
    }
    let rows = count / 8;
    Some((count - rows, rows))
}

/// The summary log's tag: the home's, its last byte turned, so an image
/// from before 1521 that reads the whole partition takes the log for
/// another home's sectors and erases them in their turn.
pub fn rows_tag(tag: [u8; 8]) -> [u8; 8] {
    let mut rows = tag;
    rows[7] ^= 0xA5;
    rows
}

/// The conversation an entry is in, as the fold files it: a received
/// message by its source (a group's copy comes from the group), a sent
/// one by its destination. Read from the packed message's head without
/// unpacking it: destination (16), source (16).
pub fn wire_peer(entry: &Entry) -> Option<[u8; 16]> {
    let at = match entry.kind {
        KIND_RECEIVED => 16,
        KIND_SENT => 0,
        _ => return None,
    };
    entry.packed.get(at..at + 16)?.try_into().ok()
}

/// What the board keeps of a conversation besides its messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    /// Its row as the model last said it; `None` until it said one.
    pub row: Option<Row>,
    /// The ring sectors its entries lie in, ascending.
    pub sectors: Vec<u32>,
    /// Entries of it in the ring.
    pub entries: u32,
}

/// Add the sectors of `span` to the ascending `sectors`.
fn add_span(sectors: &mut Vec<u32>, span: Span) {
    let (from, to) = span;
    let mut seq = from;
    loop {
        if sectors.last().is_none_or(|&last| last < seq) {
            sectors.push(seq);
        } else if let Err(at) = sectors.binary_search(&seq) {
            sectors.insert(at, seq);
        }
        if seq >= to {
            break;
        }
        seq += 1;
    }
}

/// The ring's state as the summary log keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// Where the ring ended when it was written.
    pub end: End,
    pub cursor: u64,
    pub entries: u64,
    /// The ring sectors that hold read, deleted, seen or arrived marks.
    pub marks: Vec<u32>,
    /// The asks still owed the home, oldest first.
    pub asks: Vec<PendingAsk>,
}

impl State {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.end.0.to_le_bytes());
        out.extend_from_slice(&self.end.1.to_le_bytes());
        out.extend_from_slice(&self.cursor.to_le_bytes());
        out.extend_from_slice(&self.entries.to_le_bytes());
        put_seqs(&mut out, &self.marks);
        out.extend_from_slice(&(self.asks.len() as u32).to_le_bytes());
        for ask in &self.asks {
            out.extend_from_slice(&ask.id);
            out.push(ask.tag);
            out.extend_from_slice(&(ask.request.len() as u32).to_le_bytes());
            out.extend_from_slice(&ask.request);
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<State> {
        let mut take = Take(bytes);
        let end = (take.u32()?, take.u32()?);
        let cursor = take.u64()?;
        let entries = take.u64()?;
        let marks = take.seqs()?;
        let n = take.u32()?;
        let mut asks = Vec::new();
        for _ in 0..n {
            let id: [u8; 16] = take.bytes(16)?.try_into().ok()?;
            let tag = take.bytes(1)?[0];
            let len = take.u32()? as usize;
            let request = take.bytes(len)?.to_vec();
            asks.push(PendingAsk { id, tag, request });
        }
        Some(State {
            end,
            cursor,
            entries,
            marks,
            asks,
        })
    }
}

fn put_seqs(out: &mut Vec<u8>, seqs: &[u32]) {
    out.extend_from_slice(&(seqs.len() as u32).to_le_bytes());
    for seq in seqs {
        out.extend_from_slice(&seq.to_le_bytes());
    }
}

struct Take<'a>(&'a [u8]);

impl<'a> Take<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    fn seqs(&mut self) -> Option<Vec<u32>> {
        let n = self.u32()? as usize;
        if self.0.len() < n * 4 {
            return None;
        }
        (0..n).map(|_| self.u32()).collect()
    }
}

/// What the board knows of its conversations' place in the ring
/// (1521): each conversation's [`Kept`], the names heard, the sectors
/// holding marks, and how far back each conversation is loaded into
/// the model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub kept: BTreeMap<[u8; 16], Kept>,
    pub names: BTreeMap<[u8; 16], String>,
    pub marks: Vec<u32>,
    /// Per conversation: the oldest entry sequence the model holds, and
    /// how many entries it took. A conversation not in here has none
    /// loaded.
    pub loaded: BTreeMap<[u8; 16], (u64, u32)>,
    /// Conversations whose record is owed the log.
    owed: alloc::collections::BTreeSet<[u8; 16]>,
    /// Peers whose name is owed the log.
    names_owed: alloc::collections::BTreeSet<[u8; 16]>,
}

impl Index {
    /// Take in an entry the ring holds at `span`. `live`: the model
    /// folds it too, so the conversation counts it loaded.
    pub fn take(&mut self, entry: &Entry, span: Span, live: bool) {
        match entry.kind {
            KIND_MARK => add_span(&mut self.marks, span),
            KIND_NAME => {
                if let Ok((peer, name)) = crate::home::wire::read_name_entry(&entry.packed) {
                    if self.names.get(&peer) != Some(&name) {
                        self.names.insert(peer, name);
                        self.names_owed.insert(peer);
                    }
                }
            }
            _ => {
                let Some(peer) = wire_peer(entry) else {
                    return;
                };
                let kept = self.kept.entry(peer).or_default();
                add_span(&mut kept.sectors, span);
                kept.entries += 1;
                self.owed.insert(peer);
                if live {
                    let loaded = self.loaded.entry(peer).or_insert((u64::MAX, 0));
                    loaded.0 = loaded.0.min(entry.seq);
                    loaded.1 += 1;
                }
            }
        }
    }

    /// The model said conversation `row.peer`'s row anew.
    pub fn row(&mut self, row: Row) {
        let peer = row.peer;
        let kept = self.kept.entry(peer).or_default();
        if kept.row.as_ref() != Some(&row) {
            kept.row = Some(row);
            self.owed.insert(peer);
        }
    }

    /// The model deleted conversation `peer`.
    pub fn gone(&mut self, peer: [u8; 16]) {
        self.kept.remove(&peer);
        self.loaded.remove(&peer);
        self.owed.insert(peer);
    }

    /// Every conversation counts as loaded whole: the boot folded the
    /// ring.
    pub fn all_loaded(&mut self) {
        self.loaded = self
            .kept
            .iter()
            .map(|(&p, k)| (p, (0, k.entries)))
            .collect();
    }

    /// Forget the sectors the ring took back.
    pub fn prune(&mut self, ring: &Store) {
        for kept in self.kept.values_mut() {
            kept.sectors.retain(|&seq| ring.holds(seq));
        }
        self.marks.retain(|&seq| ring.holds(seq));
    }

    /// Whether records are owed the log.
    pub fn owes(&self) -> bool {
        !self.owed.is_empty() || !self.names_owed.is_empty()
    }

    /// The records owed the log, the owed set emptied.
    fn owed_records(&mut self) -> Vec<Entry> {
        let mut out = Vec::new();
        for peer in core::mem::take(&mut self.names_owed) {
            if let Some(name) = self.names.get(&peer) {
                out.push(name_record(&peer, name));
            }
        }
        for peer in core::mem::take(&mut self.owed) {
            out.push(match self.kept.get(&peer) {
                Some(kept) => row_record(&peer, kept),
                None => record(GONE, 0, peer.to_vec()),
            });
        }
        out
    }

    /// Every live record: a snapshot's.
    fn all_records(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .names
            .iter()
            .map(|(peer, name)| name_record(peer, name))
            .collect();
        out.extend(self.kept.iter().map(|(peer, kept)| row_record(peer, kept)));
        out
    }

    /// The rows to list at boot, each with how many messages the ring
    /// holds before what the model has loaded of it.
    pub fn rows(&self) -> Vec<(Row, usize)> {
        self.kept
            .values()
            .filter_map(|kept| {
                let row = kept.row.clone()?;
                let loaded = self.loaded.get(&row.peer).map_or(0, |l| l.1);
                Some((row, kept.entries.saturating_sub(loaded) as usize))
            })
            .collect()
    }

    /// The names heard, for the model.
    pub fn names(&self) -> Vec<([u8; 16], String)> {
        self.names.iter().map(|(&p, n)| (p, n.clone())).collect()
    }

    /// The next page of conversation `peer` the model has not loaded:
    /// its `want` newest entries older than what it holds, oldest first,
    /// then the marks of the sectors from the page's oldest on (a mark
    /// is never older than the message it names), and how many older
    /// still wait in the ring. Counts the page loaded.
    pub fn page<S: Sectors>(
        &mut self,
        ring: &Store,
        dev: &mut S,
        peer: [u8; 16],
        want: usize,
    ) -> Result<(Vec<Entry>, usize), S::Error> {
        let Some(kept) = self.kept.get(&peer) else {
            return Ok((Vec::new(), 0));
        };
        let (below, had) = self.loaded.get(&peer).copied().unwrap_or((u64::MAX, 0));
        let seqs: Vec<u32> = kept
            .sectors
            .iter()
            .copied()
            .filter(|&seq| ring.holds(seq))
            .collect();
        let mut found: BTreeMap<u64, (Entry, Span)> = BTreeMap::new();
        let mut end = seqs.len();
        // Newest sectors first, four at a time, each chunk read with the
        // sector after it, so an entry that runs on across the chunk's
        // end is read whole.
        while end > 0 && found.len() < want {
            let start = end.saturating_sub(4);
            let upto = (end + 1).min(seqs.len());
            ring.read_seqs(dev, &seqs[start..upto], |entry, span| {
                if entry.seq < below && wire_peer(&entry) == Some(peer) {
                    found.insert(entry.seq, (entry, span));
                }
            })?;
            end = start;
        }
        let skip = found.len().saturating_sub(want);
        let page: Vec<(Entry, Span)> = found.into_values().skip(skip).collect();
        let mut marks: Vec<Entry> = Vec::new();
        if let Some(&(_, (from, _))) = page.first() {
            let mark_seqs: Vec<u32> = self
                .marks
                .iter()
                .copied()
                .filter(|&seq| seq >= from && ring.holds(seq))
                .collect();
            ring.read_seqs(dev, &mark_seqs, |entry, _| {
                if entry.kind == KIND_MARK {
                    marks.push(entry);
                }
            })?;
        }
        let taken = had + page.len() as u32;
        let oldest = page.first().map_or(below, |(first, _)| first.seq);
        self.loaded.insert(peer, (oldest, taken));
        let earlier = match end == 0 && skip == 0 {
            true => 0,
            false => kept.entries.saturating_sub(taken).max(1) as usize,
        };
        let mut out: Vec<Entry> = page.into_iter().map(|(entry, _)| entry).collect();
        out.extend(marks);
        Ok((out, earlier))
    }
}

fn record(kind: u8, seq: u64, packed: Vec<u8>) -> Entry {
    Entry {
        seq,
        kind,
        at: 0,
        packed,
    }
}

fn name_record(peer: &[u8; 16], name: &str) -> Entry {
    let mut packed = peer.to_vec();
    packed.extend_from_slice(name.as_bytes());
    record(NAME, 0, packed)
}

fn row_record(peer: &[u8; 16], kept: &Kept) -> Entry {
    let row = kept.row.as_ref().map(Row::encode).unwrap_or_default();
    let mut packed = Vec::with_capacity(28 + row.len() + 4 * kept.sectors.len());
    packed.extend_from_slice(peer);
    packed.extend_from_slice(&(row.len() as u32).to_le_bytes());
    packed.extend_from_slice(&row);
    packed.extend_from_slice(&kept.entries.to_le_bytes());
    put_seqs(&mut packed, &kept.sectors);
    record(ROW, 0, packed)
}

fn read_row(packed: &[u8]) -> Option<([u8; 16], Kept)> {
    let mut take = Take(packed);
    let peer: [u8; 16] = take.bytes(16)?.try_into().ok()?;
    let n = take.u32()? as usize;
    let row = match n {
        0 => None,
        _ => Some(Row::decode(take.bytes(n)?)?),
    };
    let entries = take.u32()?;
    let sectors = take.seqs()?;
    Some((
        peer,
        Kept {
            row,
            sectors,
            entries,
        },
    ))
}

/// The key a message loaded from the ring takes (1521): its entry's
/// sequence, and its place among what that entry made. A board's model
/// keys live messages above every one ([`UiModel::keys_above`] with
/// [`live_floor`]).
pub fn key(seq: u64, k: u64) -> u64 {
    (seq << 8) | k.min(0xFF)
}

/// The floor of the keys a board's model gives live messages, its ring's
/// cursor `cursor` at boot.
pub fn live_floor(cursor: u64) -> u64 {
    key(cursor.saturating_add(1), 0)
}

/// Above every key a scratch fold gives (1521, [`messages`]).
const SCRATCH: u64 = 1 << 62;

/// Conversation `peer`'s messages from a page of its entries (1521,
/// [`Index::page`]): the entries folded as the board folds them
/// (`board::fold`), in a model of their own, so a message comes out as
/// a restore that folded the whole ring made it, keyed by its entry
/// ([`key`]). What the board answers [`crate::ui::history::StoreRead::Earlier`] with.
pub fn messages<B: InputBuffer + Default>(
    entries: &[Entry],
    address: &[u8; 16],
    peer: [u8; 16],
) -> Vec<Message> {
    // On the heap: the board's UI core runs this on a census-sized stack.
    let mut scratch = alloc::boxed::Box::new(UiModel::<B>::new(&[]));
    scratch.address = Some(*address);
    scratch.home_client = true;
    scratch.list_apart = true;
    scratch.selected = crate::ui::NONE_OPEN;
    scratch.focus = crate::ui::Pane::Conversations;
    scratch.keys_above(SCRATCH);
    for entry in entries {
        let Some(event) = super::fold(entry, address) else {
            continue;
        };
        let _ = update(&mut scratch, UiEvent::Engine(event));
        let Some(conversation) = scratch.conversations.iter_mut().find(|c| c.peer == peer) else {
            continue;
        };
        let mut k = 0;
        for message in &mut conversation.messages {
            if message.activity > SCRATCH {
                message.activity = key(entry.seq, k);
                k += 1;
            }
        }
    }
    core::mem::take(&mut scratch.conversations)
        .into_iter()
        .find(|c| c.peer == peer)
        .map(|c| c.messages)
        .unwrap_or_default()
}

/// The summary log, open.
pub struct RowLog {
    store: Store,
    /// The log's sector sequence the newest snapshot began in.
    snap: Option<u32>,
    /// The newest STATE written: what a snapshot carries again.
    state: Option<State>,
}

/// What the summary log held at boot.
#[derive(Debug, Default)]
pub struct Replayed {
    pub index: Index,
    pub state: Option<State>,
    /// Records read.
    pub records: u64,
}

impl RowLog {
    /// Open the summary log of the home tagged `tag` on `dev`, and replay
    /// its records.
    pub fn open<S: Sectors>(dev: &mut S, tag: [u8; 8]) -> Result<(Self, Replayed), S::Error> {
        let mut replayed = Replayed::default();
        let mut snap = None;
        let (store, _, _) = Store::open(dev, rows_tag(tag), |entry| {
            replayed.records += 1;
            let index = &mut replayed.index;
            match entry.kind {
                ROW => {
                    if let Some((peer, kept)) = read_row(&entry.packed) {
                        index.kept.insert(peer, kept);
                    }
                }
                GONE => {
                    if let Ok(peer) = <[u8; 16]>::try_from(entry.packed.as_slice()) {
                        index.kept.remove(&peer);
                    }
                }
                NAME if entry.packed.len() >= 16 => {
                    let mut peer = [0u8; 16];
                    peer.copy_from_slice(&entry.packed[..16]);
                    if let Ok(name) = core::str::from_utf8(&entry.packed[16..]) {
                        index.names.insert(peer, String::from(name));
                    }
                }
                STATE => replayed.state = State::decode(&entry.packed),
                SNAP => snap = Some(entry.seq as u32),
                _ => {}
            }
        })?;
        let state = replayed.state.clone();
        Ok((Self { store, snap, state }, replayed))
    }

    /// Write what `index` owes, and `state` when given, in one write per
    /// sector they touch.
    pub fn write<S: Sectors>(
        &mut self,
        dev: &mut S,
        index: &mut Index,
        state: Option<&State>,
    ) -> Result<Appended, S::Error> {
        if let Some(state) = state {
            self.state = Some(state.clone());
        }
        // Three quarters of the log since the last snapshot and none
        // made in a quiet moment: this write is one, before the ring
        // comes round to it.
        if self.since_snapshot() >= self.store.sectors() * 3 / 4 {
            return self.snapshot(dev, index);
        }
        let mut records = index.owed_records();
        if let Some(state) = state {
            records.push(record(STATE, 0, state.encode()));
        }
        if records.is_empty() {
            return Ok(Appended::default());
        }
        let cursor = self.store.cursor();
        self.store.append(dev, &records, cursor)
    }

    /// The log's sectors opened since the newest snapshot began.
    fn since_snapshot(&self) -> u32 {
        match self.snap {
            None => u32::MAX,
            Some(snap) => self.store.end().0.wrapping_sub(snap),
        }
    }

    /// Whether a snapshot is due: half the log written since the last,
    /// or none made yet.
    pub fn snapshot_due(&self) -> bool {
        self.since_snapshot() >= self.store.sectors() / 2
    }

    /// Write every live record of `index` and the newest state again
    /// (1521): the log's ring may then take back every sector before
    /// it.
    pub fn snapshot<S: Sectors>(
        &mut self,
        dev: &mut S,
        index: &mut Index,
    ) -> Result<Appended, S::Error> {
        index.owed.clear();
        index.names_owed.clear();
        // The snapshot's own sector: the one the log ends in, or the next
        // when it ends full; it is said by the record's sequence.
        let at = self.store.end().0.max(1);
        let mut records = Vec::from([record(SNAP, u64::from(at), Vec::new())]);
        records.extend(index.all_records());
        if let Some(state) = self.state.as_ref() {
            records.push(record(STATE, 0, state.encode()));
        }
        let cursor = self.store.cursor();
        let done = self.store.append(dev, &records, cursor)?;
        self.snap = Some(at);
        Ok(done)
    }

    /// Erase the log's next sector in a quiet moment ([`Store::pre_erase`]).
    pub fn pre_erase<S: Sectors>(&mut self, dev: &mut S) -> Result<bool, S::Error> {
        self.store.pre_erase(dev)
    }

    /// Sectors of the log in use.
    pub fn used(&self) -> u32 {
        self.store.used()
    }
}

#[cfg(test)]
mod tests;
