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
use loquax::home::board::rows::{self, Index, RowLog, State, Window};
use loquax::home::board::store::{self, Appended, PendingAsk, Restored, Sectors, Store};
use loquax::home::wire::{Entry, Mark, Submission};
use loquax::ui::history::StoreRead;
use loquax::ui::listing::Row;
use loquax::ui::{groups, update, Batch, Pane, UiEffect, UiEvent, UiModel};

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
    behind_the_list(model, |model| {
        let mut taken = 0;
        let mut batch = Batch::new(model);
        for event in entries
            .iter()
            .filter_map(|entry| board::fold(entry, address))
        {
            batch.fold(UiEvent::Engine(event));
            taken += 1;
        }
        (taken, batch.finish())
    })
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
/// unpacking (`board::fold`), the model's steps, the batch's note, and
/// the summary log's read and write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Breakdown {
    pub read: u64,
    pub crc: u64,
    pub decode: u64,
    pub unpack: u64,
    pub step: u64,
    pub finish: u64,
    pub rows: u64,
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
}

impl Shelf {
    fn new(ring: Store, log: Option<RowLog>, index: Index, ring_count: u32) -> Self {
        Self {
            ring,
            log,
            index,
            ring_count,
            pages_owed: 0,
            dirty: false,
        }
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
    /// page kept, the state of the ring with `asks`: one flash write per
    /// sector of the summary log they touch. Nothing on a partition
    /// without a summary log.
    pub fn flush<S: Sectors>(
        &mut self,
        dev: &mut S,
        asks: impl FnOnce() -> Vec<PendingAsk>,
    ) -> Result<Appended, S::Error> {
        let Some(log) = self.log.as_mut() else {
            return Ok(Appended::default());
        };
        let state_due = self.dirty && self.pages_owed == 0;
        if !state_due && !self.index.owes() {
            return Ok(Appended::default());
        }
        self.index.prune(&self.ring);
        let state = state_due.then(|| State {
            end: self.ring.end(),
            cursor: self.ring.cursor(),
            entries: self.ring.entries(),
            marks: self.index.marks.clone(),
            asks: asks(),
        });
        let (first, count) = (self.ring_count, dev.count() - self.ring_count);
        let done = log.write(
            &mut Window::new(dev, first, count),
            &mut self.index,
            state.as_ref(),
        )?;
        if state.is_some() {
            self.dirty = false;
        }
        Ok(done)
    }

    /// The idle moment's flash work: erase ahead in the ring, then in
    /// the summary log, then a snapshot when one is due. At most one of
    /// them per call; whether one was made.
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
        if log.snapshot_due() && !self.dirty && self.pages_owed == 0 {
            log.snapshot(&mut window, &mut self.index)?;
            return Ok(true);
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
    ) -> Result<(Vec<Entry>, usize), S::Error> {
        let ring_count = self.ring_count;
        self.index
            .page(&self.ring, &mut Window::new(dev, 0, ring_count), peer, want)
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

/// What a board that is a device of the home at `address` shows at boot
/// (1476, 1521). From the summary log when its state says the ring ends
/// where it does: the rows listed, no message read, each conversation's
/// messages loaded when it opens ([`Shelf::page`]). Otherwise the ring is
/// folded whole as one batch (1514), the summary log written anew from
/// the model it made (`[STORE] index=rebuilt`), and the next boot
/// resumes. `clock` times the phases ([`Breakdown`]).
pub fn restore_timed<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    clock: &dyn Fn() -> u64,
) -> Result<Boot, S::Error> {
    let tag = store::tag(&address);
    let Some((ring_count, rows_count)) = rows::split(dev.count()) else {
        let count = dev.count();
        return rebuild(dev, address, clock, count, None, How::Whole);
    };
    let t0 = clock();
    let (log, replayed) = RowLog::open(&mut Window::new(&mut *dev, ring_count, rows_count), tag)?;
    let how = match replayed.state.as_ref() {
        None => How::Rebuilt { why: "no-state" },
        Some(state) => {
            let mut ring = Store::resume(&mut Window::new(&mut *dev, 0, ring_count), tag)?;
            if ring.end() == state.end {
                ring.resume_counts(state.cursor, state.entries);
                let mut index = replayed.index;
                index.marks = state.marks.clone();
                index.prune(&ring);
                let mut shown = model(address);
                shown.restore_rows(index.rows(), index.names());
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
                let spent = Breakdown {
                    rows: clock().saturating_sub(t0),
                    ..Breakdown::default()
                };
                return Ok(Boot {
                    model: shown,
                    shelf: Shelf::new(ring, Some(log), index, ring_count),
                    restored,
                    asks,
                    how: How::Resumed {
                        records: replayed.records,
                    },
                    spent,
                    decoded: 0,
                });
            }
            How::Rebuilt { why: "ring-moved" }
        }
    };
    let rows_read = clock().saturating_sub(t0);
    let mut boot = rebuild(dev, address, clock, ring_count, Some(log), how)?;
    boot.spent.rows += rows_read;
    Ok(boot)
}

/// The conversation a boot selects: the list's first, where the cursor
/// stands when the list shows (1521; before, the first conversation the
/// store held, wherever the list put it).
fn top(model: &UiModel<Field>) -> usize {
    model.order().first().copied().unwrap_or(0)
}

/// The ring folded whole (1476, 1514), its index built on the way, and
/// the summary log written anew from the model when there is one.
fn rebuild<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
    clock: &dyn Fn() -> u64,
    ring_count: u32,
    log: Option<RowLog>,
    how: How,
) -> Result<Boot, S::Error> {
    let mut shown = model(address);
    // No chat is on the panel at boot: the fold must not read anything
    // "where it arrived" ([`take`]'s rule; the restored read state is the
    // kept marks', nothing else's).
    shown.selected = loquax::ui::NONE_OPEN;
    let mut index = Index::default();
    let mut unpack = 0u64;
    let mut step = 0u64;
    let mut batch = Batch::new(&mut shown);
    let opened = Store::open_with(
        &mut Window::new(&mut *dev, 0, ring_count),
        store::tag(&address),
        clock,
        |entry, span| {
            index.take(&entry, span, true);
            let t0 = clock();
            let event = board::fold(&entry, &address);
            let t1 = clock();
            unpack += t1.saturating_sub(t0);
            if let Some(event) = event {
                batch.fold(UiEvent::Engine(event));
                step += clock().saturating_sub(t1);
            }
        },
    );
    let t0 = clock();
    batch.finish();
    let finish = clock().saturating_sub(t0);
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
    let mut shelf = Shelf::new(ring, log, index, ring_count);
    let t0 = clock();
    if shelf.log.is_some() {
        shelf.dirty = true;
        let pending = asks.clone();
        shelf.flush(dev, || pending)?;
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
        spent: Breakdown {
            read: spent.read,
            crc: spent.crc,
            decode: spent.decode + each,
            unpack,
            step,
            finish,
            rows,
        },
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
    let kept: Vec<Entry> = entries
        .iter()
        .filter(|entry| board::fold(entry, address).is_some())
        .cloned()
        .collect();
    let ring_count = shelf.ring_count;
    let (done, spans) =
        shelf
            .ring
            .append_spans(&mut Window::new(dev, 0, ring_count), &kept, last)?;
    for (entry, span) in kept.iter().zip(spans) {
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
    let messages = rows::messages::<Field>(entries, address, peer);
    // The cursor's chat loads while the list is up (1521: the move asks
    // its page): its first fill reads only once Enter opened it (1546;
    // before, the list's cursor moved onto an unread chat marked it read).
    behind_the_list(model, |model| {
        update(
            model,
            UiEvent::Earlier {
                peer,
                messages,
                earlier,
            },
        )
    })
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
    let mut out = Routed::default();
    for mark in model.take_marks() {
        out.verbs.push(Verb::Mark(mark.clone()));
        out.marks.push(mark);
    }
    // What the store would write (1521): the conversations the fold
    // changed, each as its row, for the summary log; the messages
    // themselves are in the ring already, entry by entry, so none is
    // copied to say which rows changed (1521b, `take_touched`).
    let touched = model.take_touched();
    out.gone = touched.gone;
    out.rows = touched
        .peers
        .iter()
        .filter_map(|peer| model.row(peer))
        .collect();
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
        UiEffect::ShowPhoto { .. } => "show-photo",
        UiEffect::OpenFile { .. } => "open-file",
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
