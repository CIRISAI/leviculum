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
//!                    u32 n, n request bytes), then 1 when marks are
//!                    filed by conversation (1556, absent before)
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
    /// The index that wrote it files a mark under the conversations it
    /// names (1556), a byte after the asks. A state without it is from
    /// an image that kept every mark in `marks`: the boot folds the ring
    /// once to place them, or every open reads them all again.
    pub placed: bool,
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
        out.push(u8::from(self.placed));
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
        // An older image wrote no byte here, and reads past it.
        let placed = take.bytes(1).is_some_and(|b| b[0] == 1);
        Some(State {
            end,
            cursor,
            entries,
            marks,
            asks,
            placed,
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
    /// Message ids this boot has seen, by their first eight bytes: the
    /// conversation each is in and the ring sector it starts in (1556).
    /// A mark naming one files its sector under that conversation
    /// ([`Index::take`]), so an open reads its own marks, not the ring's.
    ids: BTreeMap<u64, ([u8; 16], u32)>,
    /// What the last [`Index::page`] read.
    pub last_page: PageCost,
}

/// What a page read cost (1556, the `[OPEN]` line): the conversation's
/// entries it hands on, the marks among them, and the ring sectors read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageCost {
    pub messages: usize,
    pub marks: usize,
    pub sectors: usize,
}

/// A message id's key in [`Index`]'s map: its first eight bytes. Two ids
/// that share them file a mark under both conversations, which costs a
/// read, never a mark.
fn id_key(id: &[u8; 32]) -> u64 {
    let mut head = [0u8; 8];
    head.copy_from_slice(&id[..8]);
    u64::from_le_bytes(head)
}

/// The id of the message an entry carries, as the board's fold reads it
/// (`board::fold`); `None` for a mark, a name, or what the fold drops.
pub fn entry_id(entry: &Entry, address: &[u8; 16]) -> Option<[u8; 32]> {
    event_id(&super::fold(entry, address)?)
}

/// The id of the message a fold's event carries, `None` for the rest.
pub fn event_id(event: &crate::outbox::OutboxEvent) -> Option<[u8; 32]> {
    use crate::outbox::OutboxEvent;
    match event {
        OutboxEvent::Received { message_id, .. } | OutboxEvent::HomeSent { message_id, .. } => {
            Some(*message_id)
        }
        _ => None,
    }
}

impl Index {
    /// Take in an entry the ring holds at `span`. `live`: the model
    /// folds it too, so the conversation counts it loaded.
    pub fn take(&mut self, entry: &Entry, span: Span, live: bool) {
        match entry.kind {
            KIND_MARK => self.take_mark(entry, span),
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

    /// The message `id` of `entry`, at `span` of the ring, is one of
    /// its conversation's (1556): a mark that names it later files under
    /// that conversation. Learn it before the marks that name it.
    pub fn learn(&mut self, entry: &Entry, span: Span, id: &[u8; 32]) {
        if let Some(peer) = wire_peer(entry) {
            self.ids.insert(id_key(id), (peer, span.0));
        }
    }

    /// What the fold made of `entry` at `span` (1556): its message's id
    /// learnt ([`Index::learn`]), and the name of a group an invitation
    /// in it names, which the model takes from the invitation and a boot
    /// from kept rows has from no announce. Call before [`Index::take`].
    pub fn learn_event(&mut self, entry: &Entry, span: Span, event: &crate::outbox::OutboxEvent) {
        use crate::outbox::OutboxEvent;
        if let Some(id) = event_id(event) {
            self.learn(entry, span, &id);
        }
        let fields = match event {
            OutboxEvent::Received { fields, .. } | OutboxEvent::HomeSent { fields, .. } => fields,
            _ => return,
        };
        let Some(invite) = leviculum_lxmf::lev::Lev::from_fields(fields).and_then(|lev| lev.invite)
        else {
            return;
        };
        if self.names.get(&invite.group) != Some(&invite.name) {
            self.names.insert(invite.group, invite.name);
            self.names_owed.insert(invite.group);
        }
    }

    /// A mark at `span`: its sector joins the sectors of every
    /// conversation whose message it names (1556), so that conversation's
    /// page reads it with its own entries. A mark naming a message this
    /// boot has not seen, or one it cannot read, stays in [`Index::marks`],
    /// which every page reads from its oldest sector on: the old cost,
    /// for what the index cannot place.
    fn take_mark(&mut self, entry: &Entry, span: Span) {
        let ids = match crate::home::wire::read_mark(&entry.packed) {
            Ok(mark) => mark.ids,
            Err(_) => Vec::new(),
        };
        let mut unplaced = ids.is_empty();
        let mut peers: Vec<[u8; 16]> = Vec::new();
        for id in &ids {
            match self.ids.get(&id_key(id)) {
                Some(&(peer, _)) if self.kept.contains_key(&peer) => {
                    if !peers.contains(&peer) {
                        peers.push(peer);
                    }
                }
                _ => unplaced = true,
            }
        }
        for peer in peers {
            if let Some(kept) = self.kept.get_mut(&peer) {
                let before = kept.sectors.len();
                add_span(&mut kept.sectors, span);
                if kept.sectors.len() != before {
                    self.owed.insert(peer);
                }
            }
        }
        if unplaced {
            add_span(&mut self.marks, span);
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
        self.ids.retain(|_, (_, seq)| ring.holds(*seq));
    }

    /// Owe the log every conversation's record and every name again
    /// (1540, the M9's bench): the next write carries them all, as the
    /// first write after a boot that rebuilt the index does.
    pub fn owe_all(&mut self) {
        self.owed.extend(self.kept.keys().copied());
        self.names_owed.extend(self.names.keys().copied());
    }

    /// Whether records are owed the log.
    pub fn owes(&self) -> bool {
        !self.owed.is_empty() || !self.names_owed.is_empty()
    }

    /// The records owed the log, at most about `budget` bytes of them
    /// (at least one), the names first; those taken leave the owed sets
    /// (1591: a write of every row at once was 32 sector erases in one
    /// turn of the I/O core, and the UI core parked for 2.8 s of 5).
    /// `None` takes all of them.
    fn owed_records_within(&mut self, budget: Option<usize>) -> Vec<Entry> {
        let Some(budget) = budget else {
            return self.owed_records();
        };
        let mut out = Vec::new();
        let mut bytes = 0usize;
        while bytes < budget {
            if let Some(peer) = self.names_owed.pop_first() {
                if let Some(name) = self.names.get(&peer) {
                    let record = name_record(&peer, name);
                    bytes += record.packed.len();
                    out.push(record);
                }
                continue;
            }
            let Some(peer) = self.owed.pop_first() else {
                break;
            };
            let record = match self.kept.get(&peer) {
                Some(kept) => row_record(&peer, kept),
                None => record(GONE, 0, peer.to_vec()),
            };
            bytes += record.packed.len();
            out.push(record);
        }
        out
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
    /// then the marks that name one of them, and how many older still
    /// wait in the ring. Counts the page loaded.
    ///
    /// The marks come from the conversation's own sectors (a mark files
    /// there when it names a message the index knows, [`Index::take`])
    /// and from the sectors of [`Index::marks`], the ones it could not
    /// place, each from the page's oldest sector on (a mark is never
    /// older than the message it names). Before 1556 every mark of the
    /// ring from there on came along: a chat of 30 entries opened with
    /// 1730 marks, read from nearly every sector of the ring. The page's
    /// ids are learnt on the way, so a mark this board sets after the
    /// open files under the conversation too. What it read:
    /// [`Index::last_page`].
    pub fn page<S: Sectors>(
        &mut self,
        ring: &Store,
        dev: &mut S,
        peer: [u8; 16],
        want: usize,
        address: &[u8; 16],
    ) -> Result<(Vec<Entry>, usize), S::Error> {
        self.last_page = PageCost::default();
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
        let kept_entries = kept.entries;
        let mut found: BTreeMap<u64, (Entry, Span)> = BTreeMap::new();
        // The marks in the conversation's own sectors, each read once:
        // by the chunk whose own sectors it starts in.
        let mut own_marks: Vec<(Entry, Span)> = Vec::new();
        let mut sectors_read = 0usize;
        let mut end = seqs.len();
        // Newest sectors first, four at a time, each chunk read with the
        // sector after it, so an entry that runs on across the chunk's
        // end is read whole.
        while end > 0 && found.len() < want {
            let start = end.saturating_sub(4);
            let upto = (end + 1).min(seqs.len());
            let own = &seqs[start..end];
            sectors_read += upto - start;
            ring.read_seqs(dev, &seqs[start..upto], |entry, span| {
                if entry.kind == KIND_MARK {
                    if own.binary_search(&span.0).is_ok() {
                        own_marks.push((entry, span));
                    }
                } else if entry.seq < below && wire_peer(&entry) == Some(peer) {
                    found.insert(entry.seq, (entry, span));
                }
            })?;
            end = start;
        }
        let skip = found.len().saturating_sub(want);
        let page: Vec<(Entry, Span)> = found.into_values().skip(skip).collect();
        let mut ids: Vec<u64> = Vec::new();
        for (entry, span) in &page {
            if let Some(id) = entry_id(entry, address) {
                self.learn(entry, *span, &id);
                ids.push(id_key(&id));
            }
        }
        ids.sort_unstable();
        let names_page = |entry: &Entry| match crate::home::wire::read_mark(&entry.packed) {
            Ok(mark) => mark
                .ids
                .iter()
                .any(|id| ids.binary_search(&id_key(id)).is_ok()),
            Err(_) => false,
        };
        let mut marks: Vec<Entry> = Vec::new();
        if let Some(&(_, (from, _))) = page.first() {
            for (entry, span) in own_marks {
                if span.0 >= from && names_page(&entry) {
                    marks.push(entry);
                }
            }
            // The unplaced marks' sectors the conversation's own did not
            // already read.
            let mark_seqs: Vec<u32> = self
                .marks
                .iter()
                .copied()
                .filter(|&seq| seq >= from && ring.holds(seq) && seqs.binary_search(&seq).is_err())
                .collect();
            sectors_read += mark_seqs.len();
            ring.read_seqs(dev, &mark_seqs, |entry, _| {
                if entry.kind == KIND_MARK && names_page(&entry) {
                    marks.push(entry);
                }
            })?;
        }
        let taken = had + page.len() as u32;
        let oldest = page.first().map_or(below, |(first, _)| first.seq);
        self.loaded.insert(peer, (oldest, taken));
        let earlier = match end == 0 && skip == 0 {
            true => 0,
            false => kept_entries.saturating_sub(taken).max(1) as usize,
        };
        self.last_page = PageCost {
            messages: page.len(),
            marks: marks.len(),
            sectors: sectors_read,
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
///
/// `now` is the time the model the messages go into knows (1591): a
/// received message's time is its writer's, held to `now`
/// (`written_time`), and a fold that knows no time gives it none. The
/// scratch fold knew none before 1591, so every preloaded page's
/// messages came out without a time on a board that had one.
pub fn messages<B: InputBuffer + Default>(
    entries: &[Entry],
    address: &[u8; 16],
    peer: [u8; 16],
    now: Option<crate::when::WallTime>,
) -> Vec<Message> {
    messages_timed::<B>(entries, address, peer, now, &|| 0).0
}

/// [`messages`], and the time `clock` counted inside the fold's unpack
/// of the entries (`board::fold`, 1556's `[OPEN] unpack_ms`).
pub fn messages_timed<B: InputBuffer + Default>(
    entries: &[Entry],
    address: &[u8; 16],
    peer: [u8; 16],
    now: Option<crate::when::WallTime>,
    clock: &dyn Fn() -> u64,
) -> (Vec<Message>, u64) {
    let mut unpack = 0u64;
    // On the heap: the board's UI core runs this on a census-sized stack.
    let mut scratch = alloc::boxed::Box::new(UiModel::<B>::new(&[]));
    scratch.address = Some(*address);
    scratch.now = now;
    scratch.home_client = true;
    scratch.list_apart = true;
    scratch.selected = crate::ui::NONE_OPEN;
    scratch.focus = crate::ui::Pane::Conversations;
    scratch.keys_above(SCRATCH);
    for entry in entries {
        let t0 = clock();
        let event = super::fold(entry, address);
        unpack += clock().saturating_sub(t0);
        let Some(event) = event else {
            continue;
        };
        // The clock held to when the home recorded the entry (1594): the
        // time the pull's fold gave the same message.
        scratch.now = crate::ui::recorded_now(now, entry.at);
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
    let messages = core::mem::take(&mut scratch.conversations)
        .into_iter()
        .find(|c| c.peer == peer)
        .map(|c| c.messages)
        .unwrap_or_default();
    (messages, unpack)
}

/// The summary log, open.
pub struct RowLog {
    store: Store,
    /// The log's sector sequence the newest snapshot began in.
    snap: Option<u32>,
    /// The newest STATE written: what a snapshot carries again.
    state: Option<State>,
    /// A snapshot written a budget at a time (1591): the sector it began
    /// in. Its records go as owed records ([`Index::owe_all`]); its SNAP
    /// record goes last, once none is owed, so a log replayed before then
    /// still counts from the last whole snapshot.
    snapping: Option<u32>,
}

/// About how many bytes of records one bounded write of the summary log
/// takes ([`RowLog::write_within`], 1591): a sector, two erases at most
/// where it crosses into the next, about 90 ms of the UI core parked.
pub const WRITE_BUDGET: usize = super::store::SECTOR;

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
        Ok((
            Self {
                store,
                snap,
                state,
                snapping: None,
            },
            replayed,
        ))
    }

    /// Write what `index` owes, and `state` when given, in one write per
    /// sector they touch.
    pub fn write<S: Sectors>(
        &mut self,
        dev: &mut S,
        index: &mut Index,
        state: Option<&State>,
    ) -> Result<Appended, S::Error> {
        self.write_within(dev, index, state, None)
    }

    /// [`RowLog::write`] of at most about `budget` bytes of the owed
    /// records (1591, [`WRITE_BUDGET`]); the rest stay owed for the next
    /// write. `state` is written only with the last of them: a state
    /// says the rows are the ring's up to its end. A snapshot begun
    /// ([`RowLog::begin_snapshot`]) ends with the write that takes its
    /// last record. `None` writes all of them.
    pub fn write_within<S: Sectors>(
        &mut self,
        dev: &mut S,
        index: &mut Index,
        state: Option<&State>,
        budget: Option<usize>,
    ) -> Result<Appended, S::Error> {
        if let Some(state) = state {
            self.state = Some(state.clone());
        }
        // A snapshot due and none begun in a quiet moment (a pull keeps
        // the board from every quiet moment): a bounded write begins one
        // here (1594), and it and the bounded writes after it write the
        // snapshot a budget at a time, as a quiet moment's would be.
        // Window 19's board wrote it whole at three quarters instead, 645
        // records in one call, 1.5 to 1.77 s of the I/O core and the UI
        // core parked for most of it.
        if budget.is_some() && self.snapping.is_none() && self.snapshot_due() {
            self.begin_snapshot(index);
        }
        // Three quarters of the log since the last snapshot and none
        // made: this write is one, before the ring comes round to it,
        // whole; a bounded snapshot not done by then writes its rest at
        // once. Only a write without a budget, or a snapshot that took a
        // quarter of the log's ring to write, comes here.
        let late = self.since_snapshot() >= self.store.sectors() * 3 / 4;
        if late && self.snapping.is_none() {
            return self.snapshot(dev, index);
        }
        let budget = if late { None } else { budget };
        let mut records = index.owed_records_within(budget);
        let done = !index.owes();
        let snapped = match done {
            true => self.snapping.take(),
            false => None,
        };
        if let Some(at) = snapped {
            records.push(record(SNAP, u64::from(at), Vec::new()));
        }
        if done {
            if let Some(state) = state {
                records.push(record(STATE, 0, state.encode()));
            }
        }
        if records.is_empty() {
            return Ok(Appended::default());
        }
        let cursor = self.store.cursor();
        let appended = self.store.append(dev, &records, cursor)?;
        if let Some(at) = snapped {
            self.snap = Some(at);
        }
        Ok(appended)
    }

    /// Begin a snapshot written a budget at a time (1591): every live
    /// record owed again, written by the next bounded writes, the SNAP
    /// record with the last of them. Nothing is written here.
    pub fn begin_snapshot(&mut self, index: &mut Index) {
        if self.snapping.is_some() {
            return;
        }
        self.snapping = Some(self.store.end().0.max(1));
        index.owe_all();
    }

    /// A snapshot begun is still being written.
    pub fn snapping(&self) -> bool {
        self.snapping.is_some()
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
        self.snapping = None;
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

    /// Take nothing more into the log's open sector ([`Store::close`]).
    pub fn close(&mut self) {
        self.store.close();
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
