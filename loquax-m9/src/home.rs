//! The M9 as a device of its home, on the model's side (1415): the model a
//! board that joined a home starts from, the archive's pages folded into
//! it (`loquax::home::board::fold`), and the sends the model asks for
//! turned into the home's `/send`. The board's loop
//! (`leviculum_esp::handheld`) performs the asks; this is what it hands
//! the model and takes from it, `no_std` and tested on the host.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use loquax::home::ask::{GroupVerb, HomeError, Outcome, Verb};
use loquax::home::board;
use loquax::home::board::rows::{self, Index, RowLog, State, Window, Wrote};
use loquax::home::board::store::{self, Appended, PendingAsk, Restored, Sectors, Store};
use loquax::home::wire::{Entry, Mark, Submission};
use loquax::ui::history::StoreRead;
use loquax::ui::listing::Row;
use loquax::ui::{groups, update, Batch, Pane, UiEffect, UiEvent, UiModel};

use crate::bench::{Arrivals, Forgetting, BENCH_PEER};
use crate::field::Field;

/// The model of a board that is a device of the home whose LXMF address
/// is `address`: the home's identity is the board's, so what the home took
/// in is the board's own post. Empty until the first pull.
pub fn model(address: [u8; 16]) -> UiModel<Field> {
    let mut model = UiModel::<Field>::new(&[]);
    model.lang = crate::LANG;
    model.address = Some(address);
    model.focus = Pane::Conversations;
    // The board is a device of its home (1493): without this the model
    // queued no read marks (`read_out` returns early), and what was read
    // on the board stayed unread everywhere else.
    model.home_client = true;
    // The list is a screen of its own, as on a phone (1493): the M9's
    // Compact scene shows the list OR the chat, never both, so an
    // arrival while the list is up must stay unread ([`take`] hides the
    // chat from the fold while the keys are on the list).
    model.list_apart = true;
    // The board's screen (1490): the scene builds its window, not the
    // store.
    model.viewing = crate::screen::viewing(crate::font::Size::DEFAULT);
    // The board's map, as the fixture has it (1476): without it the map
    // a home board opens is sized for a terminal, not the picture.
    crate::map::ready(&mut model);
    model
}

/// Fold a page's entries into `model` as one batch (1514,
/// [`Batch`]): the list is ordered and the screen's rows built once
/// for the page, not once per entry. How many it took, and what the
/// model asks back (1493): a receipt due because the chat is open, a
/// mark — the loop routes them ([`route`]), it drops nothing.
pub fn take(
    model: &mut UiModel<Field>,
    entries: &[Entry],
    address: &[u8; 16],
) -> (usize, Vec<UiEffect>) {
    let (_, taken, effects) = fold_until(
        model,
        entries,
        address,
        &|| 0,
        &mut |_| false,
        &mut TakeSpent::default(),
    );
    (taken, effects)
}

/// Where a fold of pulled entries went, in the units of the clock it was
/// handed (1575, the bench's `[FOLD]` line): each entry unpacked into the
/// model's event (`board::fold`, the LXMF unpack and its message id), the
/// model's step for it, and the batch's note at its end (the list ordered,
/// the window built), summed over every chunk folded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TakeSpent {
    pub unpack: u64,
    pub step: u64,
    pub finish: u64,
}

/// [`take`], timed by `clock`.
pub fn take_timed(
    model: &mut UiModel<Field>,
    entries: &[Entry],
    address: &[u8; 16],
    clock: &dyn Fn() -> u64,
) -> (usize, Vec<UiEffect>, TakeSpent) {
    let mut spent = TakeSpent::default();
    let (_, taken, effects) =
        fold_until(model, entries, address, clock, &mut |_| false, &mut spent);
    (taken, effects, spent)
}

/// Fold `entries` from the first as one batch until `stop` says so after
/// an entry (it is asked with the entries looked at so far; at least one
/// is folded whatever it says): how many entries were looked at, how many
/// of them the model took, and what the batch asked back.
fn fold_until(
    model: &mut UiModel<Field>,
    entries: &[Entry],
    address: &[u8; 16],
    clock: &dyn Fn() -> u64,
    stop: &mut dyn FnMut(usize) -> bool,
    spent: &mut TakeSpent,
) -> (usize, usize, Vec<UiEffect>) {
    behind_the_list(model, |model| {
        let (mut looked, mut taken) = (0, 0);
        let mut batch = Batch::new(model);
        for entry in entries {
            let t0 = clock();
            let event = board::fold(entry, address);
            let t1 = clock();
            spent.unpack += t1.saturating_sub(t0);
            looked += 1;
            if let Some(event) = event {
                batch.fold_recorded(UiEvent::Engine(event), entry.at);
                taken += 1;
                spent.step += clock().saturating_sub(t1);
            }
            if stop(looked) {
                break;
            }
        }
        let t2 = clock();
        let effects = batch.finish();
        spent.finish += clock().saturating_sub(t2);
        (looked, taken, effects)
    })
}

/// How long one turn of the UI core folds a pulled page before it looks
/// at the keys again (1575), in microseconds from the turn's start: a
/// time, not a count of entries, because an entry's cost on the board is
/// no constant (window 16b: 234 entries in 836 ms, 55 in 536 to 620 ms,
/// the allocator and the flash's parks inside them), and the bar it
/// serves is a time. A key pressed into a turn waits for the rest of it,
/// then for the next turn's drain, its keys, its draw and its flush: the
/// bar is 100 ms from the press to the flush (order 1533). The next
/// turn's fold stops after one entry when a press is waiting
/// ([`Folding::fold`]'s `key_waiting`), so the budget bounds the turn
/// the key fell into: 25 ms of entries, then the one that crossed it and
/// the batch's note and route ([`DRAIN_BAR_MS`]). Shorter splits a page
/// into more batches, each of which notes and routes the list again
/// (host, `m9_fold_census`: about 380 allocator calls a batch however
/// few its entries); longer eats the draw's share of the 100 ms.
pub const FOLD_BUDGET_US: u64 = 25_000;

/// The longest drain stage of a UI turn the bench walk accepts (1575,
/// `scripts/m9-walk.py`'s bar for a `[LOOP] core=ui` line's `drain=`):
/// [`FOLD_BUDGET_US`], the entry that crossed it (11 ms the slowest per
/// entry window 16b showed, 600 ms for 55) and the batch's note and route
/// after it, with room for one flash park of core 0 (52 ms the longest in
/// window 16b) inside.
pub const DRAIN_BAR_MS: u64 = 100;

/// A pulled page part-folded (1575): its entries, how many of them were
/// folded already, the home's address they fold under.
struct Partial {
    entries: Vec<Entry>,
    at: usize,
    address: [u8; 16],
}

/// The pulled pages the UI core folds a budget at a time (1575): one
/// drain turn of the board folded a whole page of 234 entries, 836 ms in
/// window 16b, before it looked at the keys again; a key pressed in it
/// waited for all of it. A page goes in whole ([`Folding::begin`]) and
/// comes out folded over as many turns as its entries take
/// ([`Folding::fold`]); the page counts as folded once, when its last
/// entry is, so the summary's pages owed (`Shelf::pages_owed`) move as
/// they did.
#[derive(Default)]
pub struct Folding {
    partial: Option<Partial>,
    spent: TakeSpent,
    folded: usize,
    /// The rows and deletions the turns of a part-folded page said,
    /// held for the turn that folds its last entry ([`Folding::rows_due`]).
    rows: Vec<Row>,
    gone: Vec<[u8; 16]>,
}

/// What one [`Folding::fold`] did.
pub struct Chunk {
    /// Entries looked at in this chunk.
    pub looked: usize,
    /// What the chunk's batch asked back, for `route`.
    pub effects: Vec<UiEffect>,
    /// The page whose last entry this chunk folded, whole, for the
    /// caller's own look at it (the bench's arrival): one page folded.
    pub done: Option<Vec<Entry>>,
}

impl Folding {
    /// A page still being folded: the next turn folds on before it takes
    /// any other message.
    pub fn busy(&self) -> bool {
        self.partial.is_some()
    }

    /// Take a pulled page to fold. A page still part-folded goes on
    /// first: the caller begins a page only when [`Folding::busy`] is
    /// false, so the pages fold in the order they came.
    pub fn begin(&mut self, entries: Vec<Entry>, address: [u8; 16]) {
        debug_assert!(self.partial.is_none(), "a page begun over another");
        self.partial = Some(Partial {
            entries,
            at: 0,
            address,
        });
    }

    /// Fold the page on until `key_waiting` says a press is queued or
    /// `clock` reaches `deadline`, both asked after each entry, the press
    /// first (at least one entry is folded, so a page always moves). The
    /// entries folded go into the model as one batch.
    pub fn fold(
        &mut self,
        model: &mut UiModel<Field>,
        clock: &dyn Fn() -> u64,
        deadline: u64,
        key_waiting: &mut dyn FnMut() -> bool,
    ) -> Chunk {
        let Some(partial) = self.partial.as_mut() else {
            return Chunk {
                looked: 0,
                effects: Vec::new(),
                done: None,
            };
        };
        let (looked, _, effects) = fold_until(
            model,
            &partial.entries[partial.at..],
            &partial.address,
            clock,
            &mut |_| key_waiting() || clock() >= deadline,
            &mut self.spent,
        );
        partial.at += looked;
        self.folded += looked;
        let done = match partial.at >= partial.entries.len() {
            true => self.partial.take().map(|p| p.entries),
            false => None,
        };
        Chunk {
            looked,
            effects,
            done,
        }
    }

    /// The rows and deleted conversations a turn would hand the summary
    /// log, merged into what earlier turns held (a newer row of a
    /// conversation replaces the held one): held while a page is
    /// part-folded, so the summary log is written once a page as before
    /// (a write per turn would be a flash write, and a park of this core,
    /// per chunk). Whether they go now; then `rows` and `gone` hold all of
    /// them.
    pub fn rows_due(&mut self, rows: &mut Vec<Row>, gone: &mut Vec<[u8; 16]>) -> bool {
        self.rows
            .retain(|held| !rows.iter().any(|row| row.peer == held.peer));
        self.rows.append(rows);
        self.gone.append(gone);
        if self.busy() {
            return false;
        }
        *rows = core::mem::take(&mut self.rows);
        *gone = core::mem::take(&mut self.gone);
        true
    }

    /// Where the folds' time went since the last call, and how many
    /// entries they looked at (the bench's `[FOLD]` line).
    pub fn take_spent(&mut self) -> (TakeSpent, usize) {
        (
            core::mem::take(&mut self.spent),
            core::mem::take(&mut self.folded),
        )
    }
}

/// Run `fold` with the list's cursor hidden from it while the keys are
/// on the list. The Compact scene shows the list or the chat, never both
/// (1493): the conversation under the cursor is selected but not on the
/// panel, and what reaches it now, an arrival ([`take`]) or its page
/// from the ring ([`page_in`], 1546), must not be marked "read where it
/// arrived": the fold sees no open conversation, as a phone's closed
/// chat does. The desktop's rule stands where the chat is shown. A fold
/// that opened a conversation itself keeps it open.
fn behind_the_list<R>(
    model: &mut UiModel<Field>,
    fold: impl FnOnce(&mut UiModel<Field>) -> R,
) -> R {
    if model.focus != Pane::Conversations {
        return fold(model);
    }
    let was = model.selected;
    model.selected = loquax::ui::NONE_OPEN;
    let out = fold(model);
    if model.selected == loquax::ui::NONE_OPEN {
        model.selected = was;
    }
    out
}

/// How a boot read the message store (1521).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// From the summary log alone: the rows listed, no message read.
    Resumed {
        /// Records the summary log replayed.
        records: u64,
        /// Entries read from the ring after the summary's end (1563):
        /// none when the ring ended where the summary said.
        tail: u64,
    },
    /// The ring folded whole, and the summary log written anew from it:
    /// a store from before 1521, or one whose ring went on past its
    /// last summary (a power cut between a page and its records).
    Rebuilt { why: &'static str },
    /// A partition too small to split: the ring folded whole, no
    /// summary log.
    Whole,
}

/// Where a boot's time went, in the units of the clock it was handed
/// (1521): the ring's flash reads, CRCs and decodes, the entries'
/// unpacking (`board::fold`), the model's steps, the batch's note, the
/// drain of what the fold queued and the index's rows (1521c), and the
/// summary log's read and write. `total` is the whole boot by the same
/// clock: what the phases leave of it is time no phase names
/// ([`Breakdown::unnamed`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Breakdown {
    pub read: u64,
    pub crc: u64,
    pub decode: u64,
    pub unpack: u64,
    pub step: u64,
    pub finish: u64,
    pub drain: u64,
    pub rows: u64,
    pub total: u64,
}

impl Breakdown {
    /// The phases summed.
    pub fn named(&self) -> u64 {
        self.read
            + self.crc
            + self.decode
            + self.unpack
            + self.step
            + self.finish
            + self.drain
            + self.rows
    }

    /// What of `total` no phase names: the clock's own reads and the
    /// steps between the phases.
    pub fn unnamed(&self) -> u64 {
        self.total.saturating_sub(self.named())
    }

    /// The breakdown as the two `[STORE]` lines a boot says (1521c), its
    /// clock in microseconds, `decoded` entries read from the ring. One
    /// line outgrew the board's log line and lost its tail (`per_1000_ms
    /// read=0 crc`); two lines, each well inside it, carry every phase,
    /// the total and what no phase names. The per-1000 figures are
    /// milliseconds per 1000 entries, microseconds per entry.
    pub fn lines(&self, how: How, decoded: u64) -> [String; 2] {
        let per_k = |us: u64| us / decoded.max(1);
        let how = how.word();
        [
            alloc::format!(
                "restore.a {how} decoded={decoded} read_us={} crc_us={} decode_us={} unpack_us={} step_us={} finish_us={}",
                self.read, self.crc, self.decode, self.unpack, self.step, self.finish,
            ),
            alloc::format!(
                "restore.b {how} drain_us={} rows_us={} total_us={} unnamed_us={} per_1000_ms read={} crc={} decode={} unpack={} step={}",
                self.drain,
                self.rows,
                self.total,
                self.unnamed(),
                per_k(self.read),
                per_k(self.crc),
                per_k(self.decode),
                per_k(self.unpack),
                per_k(self.step),
            ),
        ]
    }
}

impl How {
    /// The word a `[STORE]` line says for it.
    pub fn word(&self) -> &'static str {
        match self {
            How::Resumed { .. } => "resumed",
            How::Rebuilt { .. } => "rebuilt",
            How::Whole => "whole",
        }
    }
}

/// What a board that is a device of the home at `address` shows at boot
/// (1476), and what it keeps: the model, the store's shelf, what the
/// store found, the asks still owed the home (1493), how the boot read
/// the store and where its time went.
pub struct Boot {
    pub model: UiModel<Field>,
    pub shelf: Shelf,
    pub restored: Restored,
    pub asks: Vec<PendingAsk>,
    pub how: How,
    pub spent: Breakdown,
    /// Entries the boot decoded from the ring: none when it resumed.
    pub decoded: u64,
    /// What a rebuild wrote of the summary log before it returned
    /// (1603, [`SETTLE_CALLS`]): the records, flash writes and erases of
    /// all its bounded writes, and how many writes those were. Nothing
    /// for a boot that resumed.
    pub settled: Appended,
    pub settle_calls: u32,
    /// The most sectors one of those writes erased: each stays in the
    /// writer's budget (`rows::ERASE_BUDGET`), however many there are.
    pub settle_most_erased: u32,
}

/// The board's message store (1521): the ring of entries, the summary
/// log beside it, and the index between them.
pub struct Shelf {
    pub ring: Store,
    pub log: Option<RowLog>,
    pub index: Index,
    /// The ring's sectors; the summary log's follow.
    ring_count: u32,
    /// Pages kept whose rows the model has not said yet: the summary's
    /// state waits for them, so a state never says a ring end whose
    /// rows are not written.
    pub pages_owed: u32,
    /// The ring moved since the last state written.
    pub dirty: bool,
    /// When a quiet moment's write of a backlog may go next
    /// ([`Shelf::quiet_flush`], 1591), on the caller's millisecond clock.
    backlog_at_ms: u64,
}

/// How long a quiet moment waits between two writes of a backlog of
/// summary records (1591, [`Shelf::quiet_flush`]): one sector a second,
/// its erase and its writes parking the UI core about 90 ms of it.
/// Window 18's 32 sectors at once parked it 2.8 s of 5.
pub const BACKLOG_GAP_MS: u64 = 1_000;

/// The most bounded writes a rebuild makes of its summary log before it
/// returns (1603): the whole log is 128 sectors, each write at most one.
/// Window 22's summary after a rebuild was 30-odd writes; a rebuild that
/// left them to the quiet turns, a second apart, left a state-less log
/// for 33 s, and the walks' resets inside those seconds rebuilt the ring
/// again each time (25 s a boot). A rebuild runs where nothing draws from
/// the model yet (the boot, the forget's reopen, a code taken), so the
/// writes' 1597 spacing, which was for the UI core, does not apply.
pub const SETTLE_CALLS: u32 = 4 * 128;

impl Shelf {
    fn new(ring: Store, log: Option<RowLog>, index: Index, ring_count: u32) -> Self {
        Self {
            ring,
            log,
            index,
            ring_count,
            pages_owed: 0,
            dirty: false,
            backlog_at_ms: 0,
        }
    }

    /// Whether the summary log is owed a write: records, a state due,
    /// a snapshot begun.
    pub fn owes(&self) -> bool {
        self.log.as_ref().is_some_and(|log| {
            self.index.owes() || (self.dirty && self.pages_owed == 0) || log.snapping()
        })
    }

    /// When the next quiet moment's write of a backlog may go, on the
    /// clock [`Shelf::quiet_flush`] was handed.
    pub fn backlog_at_ms(&self) -> u64 {
        self.backlog_at_ms
    }

    /// A quiet moment's write of the summary log (1591): one bounded
    /// write ([`Shelf::flush`]), and while a backlog is left
    /// ([`Shelf::owes`]), the next not before [`BACKLOG_GAP_MS`] later at
    /// `now_ms`. Nothing before then.
    pub fn quiet_flush<S: Sectors>(
        &mut self,
        dev: &mut S,
        asks: impl FnOnce() -> Vec<PendingAsk>,
        now_ms: u64,
    ) -> Result<Wrote, S::Error> {
        if now_ms < self.backlog_at_ms {
            return Ok(Wrote::Partial(Appended::default()));
        }
        let done = self.flush(dev, asks)?;
        if self.owes() {
            self.backlog_at_ms = now_ms.saturating_add(BACKLOG_GAP_MS);
        }
        Ok(done)
    }

    /// The newest sequence the ring holds.
    pub fn cursor(&self) -> u64 {
        self.ring.cursor()
    }

    /// Sectors of this home the ring holds.
    pub fn used(&self) -> u32 {
        self.ring.used()
    }

    /// Keep an ask the board owes its home ([`Store::keep_ask`]).
    pub fn keep_ask<S: Sectors>(
        &mut self,
        dev: &mut S,
        id: &[u8; 16],
        tag: u8,
        request: &[u8],
    ) -> Result<Appended, S::Error> {
        let ring_count = self.ring_count;
        self.dirty = true;
        self.ring
            .keep_ask(&mut Window::new(dev, 0, ring_count), id, tag, request)
    }

    /// The ask under `id` ended ([`Store::ask_done`]).
    pub fn ask_done<S: Sectors>(
        &mut self,
        dev: &mut S,
        id: &[u8; 16],
    ) -> Result<Appended, S::Error> {
        let ring_count = self.ring_count;
        self.dirty = true;
        self.ring.ask_done(&mut Window::new(dev, 0, ring_count), id)
    }

    /// The ring's sectors as a [`Sectors`] of their own on `dev`, the
    /// whole `messages` partition.
    pub fn ring_on<'a, S: Sectors>(&self, dev: &'a mut S) -> Window<'a, S> {
        Window::new(dev, 0, self.ring_count)
    }

    /// The model said these rows anew, and deleted these conversations.
    pub fn said(&mut self, rows: Vec<Row>, gone: Vec<[u8; 16]>) {
        for row in rows {
            self.index.row(row);
        }
        for peer in gone {
            self.index.gone(peer);
        }
    }

    /// Write the records owed and, once the model said the rows of every
    /// page kept, the state of the ring with `asks`: one bounded write of
    /// the summary log ([`RowLog::write`], 1597), at most
    /// `rows::WRITE_BUDGET` bytes in at most `rows::ERASE_BUDGET` sectors
    /// opened; what is left stays owed for the next call
    /// ([`Wrote::Partial`]), which the board's loop makes every quiet turn
    /// and after every page's rows. Every caller goes through here and
    /// none can ask for more: window 21's hang.walk found a path that
    /// still wrote everything owed at once. Nothing on a partition
    /// without a summary log.
    pub fn flush<S: Sectors>(
        &mut self,
        dev: &mut S,
        asks: impl FnOnce() -> Vec<PendingAsk>,
    ) -> Result<Wrote, S::Error> {
        let Some(log) = self.log.as_mut() else {
            return Ok(Wrote::Whole(Appended::default()));
        };
        let state_due = self.dirty && self.pages_owed == 0;
        if !state_due && !self.index.owes() && !log.snapping() {
            return Ok(Wrote::Whole(Appended::default()));
        }
        self.index.prune(&self.ring);
        let state = state_due.then(|| State {
            end: self.ring.end(),
            cursor: self.ring.cursor(),
            entries: self.ring.entries(),
            marks: self.index.marks.clone(),
            asks: asks(),
            placed: true,
        });
        let (first, count) = (self.ring_count, dev.count() - self.ring_count);
        let done = log.write(
            &mut Window::new(dev, first, count),
            &mut self.index,
            state.as_ref(),
        )?;
        // The state went with the last owed record, or waits for it.
        if state.is_some() && !done.partial() {
            self.dirty = false;
        }
        Ok(done)
    }

    /// The idle moment's flash work: erase ahead in the ring, then in
    /// the summary log, then a snapshot begun when one is due (written
    /// by the bounded flushes after, [`Shelf::flush`]). At most
    /// one erase per call; whether one was made.
    pub fn quiet<S: Sectors>(&mut self, dev: &mut S) -> Result<bool, S::Error> {
        if self
            .ring
            .pre_erase(&mut Window::new(&mut *dev, 0, self.ring_count))?
        {
            return Ok(true);
        }
        let (first, count) = (self.ring_count, dev.count() - self.ring_count);
        let Some(log) = self.log.as_mut() else {
            return Ok(false);
        };
        let mut window = Window::new(dev, first, count);
        if log.pre_erase(&mut window)? {
            return Ok(true);
        }
        // A snapshot due is begun here and written by the bounded
        // flushes that follow (1591): window 18's quiet snapshot wrote
        // 644 records, 32 sectors, in one turn.
        if log.snapshot_due() && !log.snapping() && !self.dirty && self.pages_owed == 0 {
            log.begin_snapshot(&mut self.index);
        }
        Ok(false)
    }

    /// The next page of conversation `peer` the model has not loaded
    /// ([`Index::page`]): at most `want` entries and the marks behind
    /// them, and how many older wait.
    pub fn page<S: Sectors>(
        &mut self,
        dev: &mut S,
        peer: [u8; 16],
        want: usize,
        address: &[u8; 16],
    ) -> Result<(Vec<Entry>, usize), S::Error> {
        let ring_count = self.ring_count;
        self.index.page(
            &self.ring,
            &mut Window::new(dev, 0, ring_count),
            peer,
            want,
            address,
        )
    }
}

/// The entries a conversation's first page loads when it opens (1521,
/// 1490's rule): the chat's viewport twice over, as the scene builds it,
/// in the size that shows the most rows (1532), so a switch of the size
/// never finds the page short.
pub fn page_entries() -> usize {
    let rows = crate::font::Size::ALL
        .iter()
        .map(|&size| crate::screen::viewing(size).chat_rows)
        .max()
        .unwrap_or(1);
    2 * rows.max(1)
}

/// [`restore_timed`] without a clock.
pub fn restore<S: Sectors>(dev: &mut S, address: [u8; 16]) -> Result<Boot, S::Error> {
    restore_timed(dev, address, &|| 0)
}

/// [`restore_shown`] as a release image restores: the bench's arrivals
/// dropped ([`Arrivals::Dropped`]).
pub fn restore_timed<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    clock: &dyn Fn() -> u64,
) -> Result<Boot, S::Error> {
    restore_shown(dev, address, clock, Arrivals::Dropped)
}

/// What a board that is a device of the home at `address` shows at boot
/// (1476, 1521). From the summary log when its state says the ring ends
/// where it does: the rows listed, no message read, each conversation's
/// messages loaded when it opens ([`Shelf::page`]). Otherwise the ring is
/// folded whole as one batch (1514), the summary log written anew from
/// the model it made (`[STORE] index=rebuilt`), and the next boot
/// resumes. `clock` times the phases ([`Breakdown`]). `arrivals` says
/// whether the bench's own entries are listed (1562, [`Arrivals`]): a
/// release image passes [`Arrivals::Dropped`] and never shows "Bench".
pub fn restore_shown<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    clock: &dyn Fn() -> u64,
    arrivals: Arrivals,
) -> Result<Boot, S::Error> {
    restore_chunked(dev, address, clock, arrivals, &mut |_| ())
}

/// Entries a rebuild folds between two calls of its `chunk`
/// ([`restore_chunked`]): about 2.3 s of the board's fold at window 22's
/// 4.5 ms an entry.
pub const REBUILD_CHUNK: u64 = 512;

/// [`restore_shown`], with `chunk` called after every [`REBUILD_CHUNK`]
/// entries a rebuild folded, with how many it folded so far (1603): the
/// board's `[STORE] rebuild chunk=` line, so a rebuild of 25 s shows its
/// progress. A boot that resumes never calls it.
pub fn restore_chunked<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    clock: &dyn Fn() -> u64,
    arrivals: Arrivals,
    chunk: &mut dyn FnMut(u64),
) -> Result<Boot, S::Error> {
    let tag = store::tag(&address);
    let Some((ring_count, rows_count)) = rows::split(dev.count()) else {
        let count = dev.count();
        return rebuild(
            dev,
            address,
            clock,
            count,
            None,
            How::Whole,
            arrivals,
            chunk,
        );
    };
    let t0 = clock();
    let (log, replayed) = RowLog::open(&mut Window::new(&mut *dev, ring_count, rows_count), tag)?;
    let how = match replayed.state.as_ref() {
        None => How::Rebuilt { why: "no-state" },
        // An older image's index kept every mark apart (1556): fold the
        // ring once to file them by conversation.
        Some(state) if !state.placed => How::Rebuilt {
            why: "marks-unplaced",
        },
        Some(state) => {
            let mut ring = Store::resume(&mut Window::new(&mut *dev, 0, ring_count), tag)?;
            if ring.end() == state.end {
                ring.resume_counts(state.cursor, state.entries);
                let mut index = replayed.index;
                index.marks = state.marks.clone();
                index.prune(&ring);
                let mut shown = model(address);
                let (rows, names) = listed(&index, arrivals);
                shown.restore_rows(rows, names);
                shown.selected = top(&shown);
                shown.keys_above(rows::live_floor(state.cursor));
                let _ = shown.take_marks();
                let _ = shown.take_touched();
                let restored = Restored {
                    entries: state.entries,
                    sectors: ring.used(),
                    torn: 0,
                    foreign: 0,
                };
                let asks = state.asks.clone();
                let took = clock().saturating_sub(t0);
                let spent = Breakdown {
                    rows: took,
                    total: took,
                    ..Breakdown::default()
                };
                return Ok(Boot {
                    model: shown,
                    shelf: Shelf::new(ring, Some(log), index, ring_count),
                    restored,
                    asks,
                    how: How::Resumed {
                        records: replayed.records,
                        tail: 0,
                    },
                    spent,
                    decoded: 0,
                    settled: Appended::default(),
                    settle_calls: 0,
                    settle_most_erased: 0,
                });
            }
            // The ring went on past the summary (a reset or a power cut
            // between a page and its state, 1563): the tail after the
            // summary's end is read and folded as the live pull folded
            // it, the rest stands as the summary says it.
            if let Some(boot) = resume_tail(
                dev,
                address,
                clock,
                ring_count,
                log,
                replayed.index,
                state,
                replayed.records,
                t0,
                arrivals,
            )? {
                return Ok(boot);
            }
            // The tail did not read: no summary log to hand on, the log
            // opened again for the rebuild.
            let (log, _) = RowLog::open(&mut Window::new(&mut *dev, ring_count, rows_count), tag)?;
            let rows_read = clock().saturating_sub(t0);
            let mut boot = rebuild(
                dev,
                address,
                clock,
                ring_count,
                Some(log),
                How::Rebuilt { why: "ring-moved" },
                arrivals,
                chunk,
            )?;
            boot.spent.rows += rows_read;
            boot.spent.total = clock().saturating_sub(t0);
            return Ok(boot);
        }
    };
    let rows_read = clock().saturating_sub(t0);
    let mut boot = rebuild(
        dev,
        address,
        clock,
        ring_count,
        Some(log),
        how,
        arrivals,
        chunk,
    )?;
    boot.spent.rows += rows_read;
    boot.spent.total = clock().saturating_sub(t0);
    Ok(boot)
}

/// A boot from the summary log whose ring went on past its state
/// (1563): the rows listed as the summary keeps them, then the entries
/// after the state's end ([`Store::open_after`]) folded into the model
/// and the index as the live pull folds a page ([`take`], [`keep`]), and
/// the summary written anew at the ring's end, so the next boot resumes
/// outright. What it reads grows with the tail, never with the store: a
/// watchdog reset during a pull of 4000 entries rebuilt the whole ring
/// four times over before (13:11 to 13:16 on 2026-11-08, 12.6 s to
/// 55.2 s). `None` when the tail does not read: the state's sector went
/// round the ring, or the ring ends before it; the caller rebuilds.
#[allow(clippy::too_many_arguments)]
fn resume_tail<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    clock: &dyn Fn() -> u64,
    ring_count: u32,
    log: RowLog,
    mut index: Index,
    state: &State,
    records: u64,
    t0: u64,
    arrivals: Arrivals,
) -> Result<Option<Boot>, S::Error> {
    let start = clock();
    let mut tail: Vec<(Entry, store::Span)> = Vec::new();
    let Some((mut ring, restored, tail_asks, spent, done)) = Store::open_after(
        &mut Window::new(&mut *dev, 0, ring_count),
        store::tag(&address),
        state.end,
        clock,
        |entry, span| {
            if shows(&entry, arrivals) {
                tail.push((entry, span));
            }
        },
    )?
    else {
        return Ok(None);
    };
    ring.resume_counts(
        state.cursor.max(ring.cursor()),
        state.entries + restored.entries,
    );
    index.marks = state.marks.clone();
    index.prune(&ring);
    let mut shown = model(address);
    let (listed_rows, names) = listed(&index, arrivals);
    shown.restore_rows(listed_rows, names);
    shown.keys_above(rows::live_floor(state.cursor));
    let _ = shown.take_marks();
    let _ = shown.take_touched();
    // The tail as the pull kept it: each entry in the index of its
    // conversation, loaded, and folded into the model with no chat on
    // the panel ([`rebuild`]'s rule).
    let mut unpack = 0u64;
    let mut step = 0u64;
    shown.selected = loquax::ui::NONE_OPEN;
    let mut batch = Batch::new(&mut shown);
    for (entry, span) in &tail {
        let t = clock();
        let event = board::fold(entry, &address);
        let u = clock();
        unpack += u.saturating_sub(t);
        if let Some(event) = &event {
            index.learn_event(entry, *span, event);
        }
        index.take(entry, *span, true);
        if let Some(event) = event {
            batch.fold_recorded(UiEvent::Engine(event), entry.at);
            step += clock().saturating_sub(u);
        }
    }
    let t2 = clock();
    // What the summary log cannot carry is not listed (1521c), as the
    // rebuild has it: the two boots agree.
    let _ = batch.drop_rowless();
    batch.finish();
    let finish = clock().saturating_sub(t2);
    shown.selected = top(&shown);
    // The fold's marks are the store's own entries played back.
    let _ = shown.take_marks();
    let _ = shown.take_touched();
    shown.keys_above(rows::live_floor(ring.cursor()));
    let t3 = clock();
    let peers: Vec<[u8; 16]> = shown.conversations.iter().map(|c| c.peer).collect();
    for peer in peers {
        if let Some(row) = shown.row(&peer) {
            index.row(row);
        }
    }
    let mut asks = state.asks.clone();
    asks.extend(tail_asks);
    asks.retain(|ask| !done.contains(&ask.id));
    let drain = clock().saturating_sub(t3);
    let mut shelf = Shelf::new(ring, Some(log), index, ring_count);
    let t4 = clock();
    shelf.dirty = true;
    let pending = asks.clone();
    shelf.flush(dev, || pending)?;
    let rows = clock().saturating_sub(t4) + start.saturating_sub(t0);
    let decoded = restored.entries;
    Ok(Some(Boot {
        model: shown,
        shelf,
        restored: Restored {
            entries: state.entries + restored.entries,
            sectors: restored.sectors,
            torn: restored.torn,
            foreign: restored.foreign,
        },
        asks,
        how: How::Resumed {
            records,
            tail: decoded,
        },
        spent: Breakdown {
            read: spent.read,
            crc: spent.crc,
            decode: spent.decode + spent.each,
            unpack,
            step,
            finish,
            drain,
            rows,
            total: clock().saturating_sub(t0),
        },
        decoded,
        settled: Appended::default(),
        settle_calls: 0,
        settle_most_erased: 0,
    }))
}

/// Whether a restore folds `entry` (1562): a release image leaves the
/// bench's own out.
fn shows(entry: &Entry, arrivals: Arrivals) -> bool {
    arrivals == Arrivals::Shown || !crate::bench::is_bench(entry)
}

/// The rows and names a summary lists (1562): a release image leaves
/// "Bench" out. The index keeps them: what it says is the ring's.
#[allow(clippy::type_complexity)]
fn listed(index: &Index, arrivals: Arrivals) -> (Vec<(Row, usize)>, Vec<([u8; 16], String)>) {
    let (mut rows, mut names) = (index.rows(), index.names());
    if arrivals == Arrivals::Dropped {
        rows.retain(|(row, _)| row.peer != BENCH_PEER);
        names.retain(|(peer, _)| *peer != BENCH_PEER);
    }
    (rows, names)
}

/// The conversation a boot selects: the list's first, where the cursor
/// stands when the list shows (1521; before, the first conversation the
/// store held, wherever the list put it).
fn top(model: &UiModel<Field>) -> usize {
    model.order().first().copied().unwrap_or(0)
}

/// The ring folded whole (1476, 1514), its index built on the way, and
/// the summary log written anew from the model when there is one.
/// `chunk` hears every [`REBUILD_CHUNK`] entries folded.
#[allow(clippy::too_many_arguments)]
fn rebuild<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    clock: &dyn Fn() -> u64,
    ring_count: u32,
    log: Option<RowLog>,
    how: How,
    arrivals: Arrivals,
    chunk: &mut dyn FnMut(u64),
) -> Result<Boot, S::Error> {
    let start = clock();
    let mut shown = model(address);
    // No chat is on the panel at boot: the fold must not read anything
    // "where it arrived" ([`take`]'s rule; the restored read state is the
    // kept marks', nothing else's).
    shown.selected = loquax::ui::NONE_OPEN;
    let mut index = Index::default();
    let mut unpack = 0u64;
    let mut step = 0u64;
    let mut batch = Batch::new(&mut shown);
    let mut folded = 0u64;
    let opened = Store::open_with(
        &mut Window::new(&mut *dev, 0, ring_count),
        store::tag(&address),
        clock,
        |entry, span| {
            folded += 1;
            if folded.is_multiple_of(REBUILD_CHUNK) {
                chunk(folded);
            }
            if !shows(&entry, arrivals) {
                return;
            }
            let t0 = clock();
            let event = board::fold(&entry, &address);
            let t1 = clock();
            unpack += t1.saturating_sub(t0);
            // The message's id before the entry: a mark later in the
            // ring files under its conversation (1556).
            if let Some(event) = &event {
                index.learn_event(&entry, span, event);
            }
            index.take(&entry, span, true);
            if let Some(event) = event {
                batch.fold_recorded(UiEvent::Engine(event), entry.at);
                step += clock().saturating_sub(t1);
            }
        },
    );
    let t0 = clock();
    // What the summary log cannot carry is not listed (1521c): the boot
    // that resumes from it lists the rows alone, and the two must agree.
    let _ = batch.drop_rowless();
    batch.finish();
    let finish = clock().saturating_sub(t0);
    let t0 = clock();
    shown.selected = top(&shown);
    let (ring, restored, asks, spent) = opened?;
    // The fold just said marks for what it restored as read; they are the
    // store's own entries played back, nothing the home still owes.
    let _ = shown.take_marks();
    let _ = shown.take_touched();
    shown.keys_above(rows::live_floor(ring.cursor()));
    let peers: Vec<[u8; 16]> = shown.conversations.iter().map(|c| c.peer).collect();
    for peer in peers {
        if let Some(row) = shown.row(&peer) {
            index.row(row);
        }
    }
    index.all_loaded();
    let drain = clock().saturating_sub(t0);
    let mut shelf = Shelf::new(ring, log, index, ring_count);
    let t0 = clock();
    let mut settled = Appended::default();
    let mut settle_calls = 0;
    let mut settle_most_erased = 0;
    if shelf.log.is_some() {
        shelf.dirty = true;
        // The whole summary, its state last, in bounded writes with the
        // caller's clock (its watchdog and its line, 1603) between: a
        // reset after this boot resumes.
        while settle_calls < SETTLE_CALLS {
            let pending = asks.clone();
            let done = shelf.flush(dev, || pending)?;
            let wrote = done.appended();
            settled.entries += wrote.entries;
            settled.writes += wrote.writes;
            settled.erased += wrote.erased;
            settle_most_erased = settle_most_erased.max(wrote.erased);
            settle_calls += 1;
            if !shelf.owes() {
                break;
            }
            let _ = clock();
        }
    }
    let rows = clock().saturating_sub(t0);
    // What the open counted as the caller's work holds the unpack and
    // the step, counted apart; the rest is the index's.
    let each = spent.each.saturating_sub(unpack + step);
    Ok(Boot {
        model: shown,
        shelf,
        decoded: restored.entries,
        restored,
        asks,
        how,
        settled,
        settle_calls,
        settle_most_erased,
        spent: Breakdown {
            read: spent.read,
            crc: spent.crc,
            decode: spent.decode + each,
            unpack,
            step,
            finish,
            drain,
            rows,
            total: clock().saturating_sub(start),
        },
    })
}

/// What `bench forget` did (1562): the ring's entries taken out, by what
/// they were, and the summary log's sectors erased so the next boot
/// rebuilds from the ring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Forgot {
    pub store: store::Forgot,
    /// The bench's flagged entries (messages and the name).
    pub flagged: u64,
    /// 1540's arrivals, before the flag.
    pub unflagged: u64,
    /// Read marks of nothing but arrivals.
    pub marks: u64,
    /// Summary log sectors erased: all of them when anything went, none
    /// otherwise.
    pub summary_erased: u32,
}

/// Take the bench's arrivals out of the message store of the home at
/// `address` on `dev` ([`Forgetting`], `Store::forget`), and when any
/// went, erase the summary log: its rows still say "bench arrival" in a
/// chat's last line and count entries no longer there, and a boot with
/// no summary rebuilds the list from the ring, which is the truth. The
/// board restarts after it; nothing in memory stands for the store any
/// more. `feed` is called per sector, for the caller's watchdog.
pub fn forget<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    feed: &dyn Fn(),
) -> Result<Forgot, S::Error> {
    let count = dev.count();
    let (ring_count, rows_count) = rows::split(count).unwrap_or((count, 0));
    let mut forgetting = Forgetting::default();
    let store = Store::forget(
        &mut Window::new(&mut *dev, 0, ring_count),
        store::tag(&address),
        |entry| forgetting.drops(entry),
        feed,
    )?;
    let mut summary_erased = 0;
    if store.entries > 0 {
        let mut log = Window::new(dev, ring_count, rows_count);
        for sector in 0..rows_count {
            feed();
            log.erase(sector)?;
            summary_erased += 1;
        }
    }
    Ok(Forgot {
        store,
        flagged: forgetting.flagged,
        unflagged: forgetting.unflagged,
        marks: forgetting.marks,
        summary_erased,
    })
}

/// Keep a pulled page in `shelf`: the entries the board's fold keeps
/// (`board::fold`, what the list and a chat show), and the page's newest
/// sequence as the store's end, in one write per sector it touches, each
/// entry in the index of its conversation. An empty page writes nothing.
pub fn keep<S: Sectors>(
    shelf: &mut Shelf,
    dev: &mut S,
    entries: &[Entry],
    address: &[u8; 16],
) -> Result<Appended, S::Error> {
    let Some(last) = entries.iter().map(|entry| entry.seq).max() else {
        return Ok(Appended::default());
    };
    let mut kept: Vec<Entry> = Vec::new();
    let mut events = Vec::new();
    for entry in entries {
        if let Some(event) = board::fold(entry, address) {
            events.push(event);
            kept.push(entry.clone());
        }
    }
    let ring_count = shelf.ring_count;
    let (done, spans) =
        shelf
            .ring
            .append_spans(&mut Window::new(dev, 0, ring_count), &kept, last)?;
    for ((entry, span), event) in kept.iter().zip(spans).zip(&events) {
        shelf.index.learn_event(entry, span, event);
        shelf.index.take(entry, span, true);
    }
    shelf.dirty = true;
    Ok(done)
}

/// A conversation's page as the model takes it (1521): the entries
/// [`Shelf::page`] read, folded into messages as a restore folds them
/// (`board::rows::messages`), and handed to the model as the store's
/// answer to its page read. What the model asks back goes through
/// [`route`].
pub fn page_in(
    model: &mut UiModel<Field>,
    peer: [u8; 16],
    entries: &[Entry],
    earlier: usize,
    address: &[u8; 16],
) -> Vec<UiEffect> {
    page_in_timed(model, peer, entries, earlier, address, &|| 0).0
}

/// Where [`page_in`]'s time went, in `clock`'s unit (1556, the bench's
/// `[OPEN]` line): the entries decoded into messages, the unpack inside
/// that decode, and the model's fold of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageSpent {
    pub decode: u64,
    pub unpack: u64,
    pub fold: u64,
}

/// [`page_in`], timed by `clock`.
pub fn page_in_timed(
    model: &mut UiModel<Field>,
    peer: [u8; 16],
    entries: &[Entry],
    earlier: usize,
    address: &[u8; 16],
    clock: &dyn Fn() -> u64,
) -> (Vec<UiEffect>, PageSpent) {
    let t0 = clock();
    let (messages, unpack) =
        rows::messages_timed::<Field>(entries, address, peer, model.now, clock);
    let t1 = clock();
    // The cursor's chat loads while the list is up (1521: the move asks
    // its page): its first fill reads only once Enter opened it (1546;
    // before, the list's cursor moved onto an unread chat marked it read).
    let effects = behind_the_list(model, |model| {
        update(
            model,
            UiEvent::Earlier {
                peer,
                messages,
                earlier,
            },
        )
    });
    let spent = PageSpent {
        decode: t1.saturating_sub(t0),
        unpack,
        fold: clock().saturating_sub(t1),
    };
    (effects, spent)
}

/// What a fold's output becomes on the board (1493): the verbs for the
/// ask queue, the marks among them (kept locally too, [`keep_marks`]),
/// the lines for the log, and what the loop does itself. Nothing is
/// dropped silently: an effect the board has no platform for is named.
#[derive(Debug, Default)]
pub struct Routed {
    /// For the ask queue, oldest first: sends, marks, group verbs.
    pub verbs: Vec<Verb>,
    /// The marks among the verbs, as the model said them: written into
    /// the message store as well, so read state survives a reboot that
    /// comes before the home heard it.
    pub marks: Vec<Mark>,
    /// Lines for the debug log: the model's notices and log asks.
    pub notices: Vec<String>,
    /// Effects the board cannot perform, each by name, for the log.
    pub unrouted: Vec<&'static str>,
    /// The screen owes a full redraw (`Ctrl-l`).
    pub redraw: bool,
    /// The rows of the conversations the fold changed (1521), for the
    /// summary log.
    pub rows: Vec<Row>,
    /// The conversations the fold deleted.
    pub gone: Vec<[u8; 16]>,
    /// The conversations whose next page the model asks (1521, the
    /// store's page read): the I/O core reads it from the ring.
    pub loads: Vec<[u8; 16]>,
}

/// Route everything a fold left behind (1493): `effects` as [`update`]
/// returned them, and the queues the desktop's shell drains each fold
/// (`take_marks`, the store writes as `take_touched`, petnames, peer
/// states), which grew until reboot on a board that never drained them. Marks become `/mark` asks;
/// a send and a group verb their asks, each under a fresh id from `ids`
/// (the same id asked again is the same message at the home). Store
/// writes are drained and dropped: the message store in flash already
/// holds what they would write, entry by entry. Petnames and peer states
/// have no ask on the board yet and are named in `unrouted`.
pub fn route(
    model: &mut UiModel<Field>,
    effects: Vec<UiEffect>,
    ids: &mut impl FnMut() -> [u8; 16],
) -> Routed {
    route_rows(model, effects, ids, true)
}

/// [`route`] for a chunk of a pulled page that is not its last
/// ([`Folding::fold`], 1591): the rows the chunk changed stay owed in the
/// model ([`UiModel::take_touched`]) for the route after the page's last
/// chunk, which builds each once. The summary log is written once a
/// page anyway ([`Folding::rows_due`]); before 1591 every chunk built
/// the rows of the conversations it touched, and the next chunk's
/// replaced them (`m9_fold_census`: 431 allocator calls in twelve chunks
/// of one page against 65 for the page whole).
pub fn route_held(
    model: &mut UiModel<Field>,
    effects: Vec<UiEffect>,
    ids: &mut impl FnMut() -> [u8; 16],
) -> Routed {
    route_rows(model, effects, ids, false)
}

fn route_rows(
    model: &mut UiModel<Field>,
    effects: Vec<UiEffect>,
    ids: &mut impl FnMut() -> [u8; 16],
    rows: bool,
) -> Routed {
    let mut out = Routed::default();
    // The home never had the bench's arrivals (1562): their marks are
    // kept on the board and never asked of it.
    let bench = crate::bench::ids(model);
    for mark in model.take_marks() {
        let home = Mark {
            flag: mark.flag,
            ids: mark
                .ids
                .iter()
                .filter(|id| !bench.contains(*id))
                .copied()
                .collect(),
        };
        if !home.ids.is_empty() {
            out.verbs.push(Verb::Mark(home));
        }
        out.marks.push(mark);
    }
    // What the store would write (1521): the conversations the fold
    // changed, each as its row, for the summary log; the messages
    // themselves are in the ring already, entry by entry, so none is
    // copied to say which rows changed (1521b, `take_touched`).
    if rows {
        let touched = model.take_touched();
        out.gone = touched.gone;
        out.rows = touched
            .peers
            .iter()
            .filter_map(|peer| model.row(peer))
            .collect();
    }
    if !model.take_petnames().is_empty() {
        out.unrouted.push("petnames");
    }
    if !model.take_peer_states().is_empty() {
        out.unrouted.push("peer-states");
    }
    for effect in effects {
        match effect {
            UiEffect::CreateGroup { .. }
            | UiEffect::AddMembers { .. }
            | UiEffect::DeleteGroup { .. } => {
                if let Some(verb) = crate::group::ask_of(&effect, ids()) {
                    out.verbs.push(Verb::Group(verb));
                }
            }
            UiEffect::Send { .. } => {
                let id = ids();
                match crate::group::ask_of(&effect, id) {
                    Some(verb) => out.verbs.push(Verb::Group(verb)),
                    None => {
                        let UiEffect::Send { to, body, fields } = effect else {
                            unreachable!()
                        };
                        out.verbs.push(Verb::Send(Submission {
                            client_id: id,
                            destination: to,
                            title: Vec::new(),
                            content: body.into_bytes(),
                            fields,
                        }));
                    }
                }
            }
            UiEffect::Notice(line) | UiEffect::Log(line) => out.notices.push(line),
            UiEffect::PnLine(line) => out.notices.push(line),
            UiEffect::Redraw => out.redraw = true,
            UiEffect::Store(StoreRead::Earlier { peer, .. }) => {
                if !out.loads.contains(&peer) {
                    out.loads.push(peer);
                }
            }
            other => out.unrouted.push(effect_name(&other)),
        }
    }
    out
}

/// The word an effect the board cannot perform is logged under.
fn effect_name(effect: &UiEffect) -> &'static str {
    match effect {
        UiEffect::Network { .. } => "network",
        UiEffect::Send { .. } => "send",
        UiEffect::Fetch => "fetch",
        UiEffect::AskPages => "ask-pages",
        UiEffect::AskPn { .. } => "ask-pn",
        UiEffect::PnLine(_) => "pn-line",
        UiEffect::Quit => "quit",
        UiEffect::Redraw => "redraw",
        UiEffect::Copy(_) => "copy",
        UiEffect::Browse(_) => "browse",
        UiEffect::Rooms(_) => "rooms",
        UiEffect::Contact(_) => "contact",
        UiEffect::Open(_) => "open",
        UiEffect::Mouse(_) => "mouse",
        UiEffect::Notice(_) => "notice",
        UiEffect::Store(_) => "store-read",
        UiEffect::Log(_) => "log",
        UiEffect::LoadMap(_) => "load-map",
        UiEffect::MapDrawing(_) => "map-drawing",
        UiEffect::Import => "import",
        UiEffect::Remember { .. } => "remember",
        UiEffect::RememberHash { .. } => "remember-hash",
        UiEffect::ShowPhoto { .. } => "show-photo",
        UiEffect::OpenFile { .. } => "open-file",
        UiEffect::ForgetSaved { .. } => "forget-saved",
        UiEffect::PlayVoice { .. } => "play-voice",
        UiEffect::PauseVoice { .. } => "pause-voice",
        UiEffect::CreateGroup { .. } => "create-group",
        UiEffect::AddMembers { .. } => "add-members",
        UiEffect::DeleteGroup { .. } => "delete-group",
        UiEffect::DeleteAllMaps => "delete-all-maps",
        UiEffect::Attach { .. } => "attach",
        UiEffect::ScanCode { .. } => "scan-code",
        UiEffect::Recording { .. } => "recording",
    }
}

/// Keep the board's own marks in the message store (1493): each as a
/// [`loquax::home::KIND_MARK`] entry under sequence 0, which the restore
/// folds back the way a pulled mark folds, so what was read here is read
/// after a reboot — home heard from or not. The store's cursor does not
/// move. Folding the home's archived copy of the same mark later is
/// idempotent.
pub fn keep_marks<S: Sectors>(
    shelf: &mut Shelf,
    dev: &mut S,
    marks: &[Mark],
) -> Result<Appended, S::Error> {
    if marks.is_empty() {
        return Ok(Appended::default());
    }
    let entries: Vec<Entry> = marks
        .iter()
        .map(|mark| Entry {
            seq: 0,
            kind: loquax::home::KIND_MARK,
            at: 0,
            packed: loquax::home::wire::mark_request(mark),
        })
        .collect();
    let cursor = shelf.ring.cursor();
    let ring_count = shelf.ring_count;
    let (done, spans) =
        shelf
            .ring
            .append_spans(&mut Window::new(dev, 0, ring_count), &entries, cursor)?;
    for (entry, span) in entries.iter().zip(spans) {
        shelf.index.take(entry, span, false);
    }
    shelf.dirty = true;
    Ok(done)
}

/// The verbs waiting for the home, oldest first, asked one at a time,
/// each under the id its store frame carries (1493).
#[derive(Debug, Default)]
pub struct Outgoing {
    queue: VecDeque<([u8; 16], Verb)>,
}

impl Outgoing {
    pub fn push(&mut self, id: [u8; 16], verb: Verb) {
        self.queue.push_back((id, verb));
    }

    /// The oldest, still queued until [`Outgoing::done`].
    pub fn next(&self) -> Option<&([u8; 16], Verb)> {
        self.queue.front()
    }

    /// The oldest was asked and is over: taken or given up.
    pub fn done(&mut self) -> Option<([u8; 16], Verb)> {
        self.queue.pop_front()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The queue as the summary log keeps it (1521): every verb that has
    /// an ask frame, oldest first.
    pub fn pending(&self) -> Vec<PendingAsk> {
        self.queue
            .iter()
            .filter_map(|(id, verb)| {
                board::ask_bytes(verb).map(|(tag, request)| PendingAsk {
                    id: *id,
                    tag,
                    request,
                })
            })
            .collect()
    }
}

/// How an ask ended, carried from the client's executor to the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// A send: the message id the home gave it, or why not.
    Sent(Result<[u8; 32], String>),
    /// A mark: nothing to show; an error is a line.
    Marked(Result<(), String>),
    /// `/group`: the group's address, or why it was not made.
    GroupMade(Result<[u8; 16], String>),
    /// `/groupadd`.
    MembersAdded(Result<[u8; 16], String>),
    /// `/groupdelete`.
    GroupDeleted(Result<[u8; 16], String>),
}

/// What `result` answers the `verb` that was asked; `None` for a verb
/// the queue never carries (a join, a pull).
pub fn answer_of(verb: &Verb, result: &Result<Outcome, HomeError>) -> Option<Answer> {
    let error = |e: &HomeError| alloc::format!("{e}");
    Some(match verb {
        Verb::Send(_) | Verb::Group(GroupVerb::State(_)) => Answer::Sent(match result {
            Ok(Outcome::Sent(id)) => Ok(*id),
            Ok(other) => Err(alloc::format!("{other:?}")),
            Err(e) => Err(error(e)),
        }),
        Verb::Mark(_) => Answer::Marked(match result {
            Ok(_) => Ok(()),
            Err(e) => Err(error(e)),
        }),
        Verb::Group(GroupVerb::Create(_)) => Answer::GroupMade(group_result(result)),
        Verb::Group(GroupVerb::Add(_)) => Answer::MembersAdded(group_result(result)),
        Verb::Group(GroupVerb::Delete(delete)) => Answer::GroupDeleted(match result {
            Ok(_) => Ok(delete.group),
            Err(e) => Err(error(e)),
        }),
        Verb::Join { .. } | Verb::Pull { .. } => return None,
    })
}

fn group_result(result: &Result<Outcome, HomeError>) -> Result<[u8; 16], String> {
    match result {
        Ok(Outcome::Group(group)) => Ok(*group),
        Ok(other) => Err(alloc::format!("{other:?}")),
        Err(e) => Err(alloc::format!("{e}")),
    }
}

/// Fold `answer` into `model`: the bubble named, the group screen told,
/// as the desktop's runner tells them. What the model asks back goes
/// through [`route`] like any fold's effects.
pub fn answered(model: &mut UiModel<Field>, answer: Answer) -> Vec<UiEffect> {
    match answer {
        Answer::Sent(Ok(message_id)) => {
            let mut effects = update(
                model,
                UiEvent::Engine(loquax::outbox::OutboxEvent::Queued { message_id }),
            );
            effects.extend(update(model, UiEvent::SendDone(Ok(()))));
            effects
        }
        Answer::Sent(Err(e)) => update(model, UiEvent::SendDone(Err(e))),
        Answer::Marked(_) => Vec::new(),
        Answer::GroupMade(made) => update(
            model,
            UiEvent::GroupMade(made.map_err(groups::NotMade::Failed)),
        ),
        Answer::MembersAdded(added) => update(model, UiEvent::MembersAdded(added)),
        Answer::GroupDeleted(deleted) => update(model, UiEvent::GroupDeleted(deleted)),
    }
}
