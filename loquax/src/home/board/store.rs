//! The board's message store in flash (1476): the entries it pulled and
//! its fold keeps (`board::fold`), so a board that loses power shows its
//! chats again at boot and its cursor and its store agree
//! (`board::agree`). `no_std`; the flash is a [`Sectors`], which the M9
//! implements over its `messages` partition and the host tests in memory.
//!
//! An append-only log in the part's 4 KiB erase sectors, used as a ring:
//! a full sector closes, the next one in the ring is erased and opened,
//! and once the ring is round the oldest sector is the one erased. Each
//! sector starts with a header, written once when it is opened:
//!
//! ```text
//! offset  len  field
//!      0    4  magic "LQXM"
//!      4    1  version, 1
//!      5    3  zero
//!      8    4  the sector's sequence, little end first: one more than the
//!             newest sector's when it was opened
//!     12    8  the home's tag: the first eight bytes of its LXMF address
//!     20    4  CRC-32 over bytes 0..20
//! ```
//!
//! then frames, each written into erased flash and never rewritten:
//!
//! ```text
//!      0    2  the payload's length, little end first
//!      2    1  flags: 1 an entry's first part, 2 its last, 4 a cursor mark,
//!             8 a pending ask, 16 an ask that ended (1493)
//!      3    1  zero
//!      4    4  CRC-32 over bytes 0..4 and the payload
//!      8    n  the payload, zero padded to a multiple of four
//! ```
//!
//! An entry is `seq` (8), `kind` (1), `at` (8) and the packed message; one
//! too long for the room a sector has left goes on as further parts in
//! the next sector. A cursor mark is the newest sequence of a page (8):
//! a page whose entries the fold kept none of still moves the store's
//! end, so the cursor does not pull it again.
//!
//! An ask frame (1493) is a request the board owes its home — a send, a
//! mark, a group's verb — kept when it is queued so a power cycle loses
//! none: its id (16), the verb's tag (1) and the request's wire bytes,
//! split across sectors as an entry is. A done frame is the id (16) of an
//! ask that ended. At boot the asks without a done are handed back
//! oldest first ([`PendingAsk`]), and the board queues them again; every
//! request carries a client id or marks idempotently, so one that was
//! answered while its done was lost is asked again without harm.
//!
//! A power loss leaves at most a frame that does not check: the scan
//! stops at it, the sector is closed, and the next page opens a fresh
//! sector. A sector whose header does not check is not read. Sectors of
//! another home's tag are not shown and are erased in their turn.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::home::wire::Entry;

/// The part's erase unit.
pub const SECTOR: usize = 4096;
/// A sector's header.
pub const HEADER: usize = 24;
/// A frame's header.
pub const FRAME: usize = 8;
/// The most payload one frame carries: a whole sector after its header.
pub const MAX_PART: usize = SECTOR - HEADER - FRAME;

const MAGIC: &[u8; 4] = b"LQXM";
const VERSION: u8 = 1;
const FIRST: u8 = 1;
const LAST: u8 = 2;
const MARK: u8 = 4;
/// A pending ask (1493): a request the board owes its home.
const ASK: u8 = 8;
/// An ask that ended: its id, so the boot does not queue it again.
const DONE: u8 = 16;
/// The flags that say a frame is no entry part.
const SPECIAL: u8 = MARK | ASK | DONE;
/// An entry's fixed fields before its packed bytes.
const ENTRY_HEAD: usize = 17;
/// Below this much room a sector is closed rather than split into: a part
/// smaller than a frame's header is not worth its header.
const LEAST_PART: usize = 16;

/// The flash a store lives in: `count` sectors of [`SECTOR`] bytes, read
/// anywhere, written only where erased, offsets and lengths a multiple of
/// four, erased a sector at a time.
pub trait Sectors {
    type Error: core::fmt::Debug;
    /// How many sectors there are.
    fn count(&self) -> u32;
    /// Read `buf.len()` bytes at `offset` of `sector`.
    fn read(&mut self, sector: u32, offset: usize, buf: &mut [u8]) -> Result<(), Self::Error>;
    /// Write `data` at `offset` of `sector`, which is erased there.
    fn write(&mut self, sector: u32, offset: usize, data: &[u8]) -> Result<(), Self::Error>;
    /// Erase `sector`: every byte 0xFF.
    fn erase(&mut self, sector: u32) -> Result<(), Self::Error>;
}

/// What the store found at boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Restored {
    /// Entries handed back, whole.
    pub entries: u64,
    /// Sectors of this home that were read.
    pub sectors: u32,
    /// Frames that did not check: the end of a write a power loss cut.
    pub torn: u32,
    /// Sectors of another home, skipped.
    pub foreign: u32,
}

/// What one page's write did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Appended {
    /// Entries written.
    pub entries: u64,
    /// Flash writes made: one per sector the page touched.
    pub writes: u32,
    /// Sectors erased to open them.
    pub erased: u32,
}

/// What [`Store::forget`] took out (1562).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Forgot {
    /// Entries taken out.
    pub entries: u64,
    /// Their frames: more than the entries where one ran on into the
    /// next sector.
    pub frames: u32,
    /// Sectors written again.
    pub sectors: u32,
}

/// An ask restored at boot (1493): queued before the power went, no done
/// frame behind it. The board queues it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAsk {
    /// The id its done frame will carry.
    pub id: [u8; 16],
    /// The verb's tag, as the board gave it ([`Store::keep_ask`]).
    pub tag: u8,
    /// The request's wire bytes.
    pub request: Vec<u8>,
}

/// The sectors an entry's frames lie in (1521): the sequence of the
/// sector its first part is in and of the one its last part is in, the
/// same for an entry that did not split. What the per-conversation index
/// keeps, so a conversation's entries are read from its sectors alone.
pub type Span = (u32, u32);

/// Where a restore's time went (1521), in the units of the clock the
/// caller handed [`Store::open_with`]: the flash reads, the CRC checks,
/// the entries decoded, and the caller's own work per entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spent {
    pub read: u64,
    pub crc: u64,
    pub decode: u64,
    pub each: u64,
}

/// Where the log ends (1521): the newest sector's sequence and the offset
/// after its last frame. A summary written with it is current exactly
/// when the boot finds the log ending at the same place
/// ([`Store::resume`]).
pub type End = (u32, u32);

/// The log: where it ends, how far it reaches, and where the next frame
/// goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    tag: [u8; 8],
    count: u32,
    /// The newest sector of any home, its index and sequence.
    newest: Option<(u32, u32)>,
    /// Where the next frame goes: the newest sector's index and the
    /// offset of its first erased byte, while it is this home's and open.
    tail: Option<(u32, usize)>,
    /// The newest sequence the store holds, entry or mark.
    cursor: u64,
    /// Entries held.
    entries: u64,
    /// Sectors of this home.
    used: u32,
    /// Sectors erased since boot.
    erases: u64,
    /// The ring's next sector, when it is known erased (1493): the
    /// idle-time job ([`Store::pre_erase`]) erased it, or found it so, and
    /// the page path writes into it without holding the line for an erase.
    ready: Option<u32>,
    /// This home's sectors, by their sequence (1521): what a
    /// conversation's index of sequences is read through.
    seqs: BTreeMap<u32, u32>,
    /// Where the log ends ([`End`]).
    end: End,
    /// Sectors opened since boot (1521): what a summary log counts its
    /// snapshots by.
    opened: u64,
}

/// The tag of the home whose LXMF address is `address`.
pub fn tag(address: &[u8; 16]) -> [u8; 8] {
    let mut tag = [0u8; 8];
    tag.copy_from_slice(&address[..8]);
    tag
}

impl Store {
    /// Open the store of the home tagged `tag` on `dev`, and hand every
    /// entry it holds to `each`, oldest first. The third of the answer is
    /// the asks still owed the home (1493): kept when they were queued,
    /// no done frame behind them, oldest first.
    pub fn open<S: Sectors>(
        dev: &mut S,
        tag: [u8; 8],
        mut each: impl FnMut(Entry),
    ) -> Result<(Self, Restored, Vec<PendingAsk>), S::Error> {
        let opened = Self::open_with(dev, tag, &|| 0, |entry, _| each(entry))?;
        Ok((opened.0, opened.1, opened.2))
    }

    /// [`Store::open`], each entry handed with its [`Span`], and the time
    /// the open spent by `clock` (1521): the flash reads, the CRCs, the
    /// decode and the caller's `each`.
    pub fn open_with<S: Sectors>(
        dev: &mut S,
        tag: [u8; 8],
        clock: &dyn Fn() -> u64,
        each: impl FnMut(Entry, Span),
    ) -> Result<(Self, Restored, Vec<PendingAsk>, Spent), S::Error> {
        let (store, restored, asks, spent, _) = Self::scan(dev, tag, None, clock, each)?;
        Ok((store, restored, asks, spent))
    }

    /// Open the log of the home tagged `tag` on `dev` and read only what
    /// it holds after `from` (1563): a summary written at `from` stands
    /// for everything before it, so a boot that finds the ring past its
    /// summary reads the tail a reset or a power cut left unsummarised,
    /// not the whole ring again. `each` gets the tail's entries with
    /// their spans, oldest first. `None` when `from` is no place in this
    /// log: its sector went round the ring, or the log ends before it.
    ///
    /// The store's cursor and entry count are the tail's alone: the
    /// caller adds the summary's ([`Store::resume_counts`]). The asks are
    /// the tail's owed ones; the ids are every done frame the tail holds,
    /// which may end an ask the summary kept.
    #[allow(clippy::type_complexity)]
    pub fn open_after<S: Sectors>(
        dev: &mut S,
        tag: [u8; 8],
        from: End,
        clock: &dyn Fn() -> u64,
        each: impl FnMut(Entry, Span),
    ) -> Result<Option<(Self, Restored, Vec<PendingAsk>, Spent, Vec<[u8; 16]>)>, S::Error> {
        // Where the log ends, read as a resume reads it: a scan begun
        // at `from` would take any offset past the end for a frame's start.
        let log = Self::resume(dev, tag)?;
        if !log.seqs.contains_key(&from.0) || log.end < from {
            return Ok(None);
        }
        let opened = Self::scan(dev, tag, Some(from), clock, each)?;
        // A scan that ended elsewhere began off a frame's start.
        if opened.0.end != log.end {
            return Ok(None);
        }
        Ok(Some(opened))
    }

    /// Take the entries `drop` names out of the log of the home tagged
    /// `tag` on `dev` (1562, the bench's `bench forget`): every sector
    /// that holds a frame of one is read, erased and written again with
    /// its other frames in their order, its header as it was. Entries
    /// are handed to `drop` oldest first, whole, as a boot reads them;
    /// cursor marks, asks and dones are never dropped. `feed` is called
    /// once per sector read and once per sector written, for a caller's
    /// watchdog.
    ///
    /// What the log said before is no longer where it was: a summary
    /// written for it is stale, and the caller drops it so the next boot
    /// rebuilds. A power loss between a sector's erase and its write
    /// loses that sector's entries, as a ring that went round does.
    pub fn forget<S: Sectors>(
        dev: &mut S,
        tag: [u8; 8],
        mut drop: impl FnMut(&Entry) -> bool,
        feed: &dyn Fn(),
    ) -> Result<Forgot, S::Error> {
        let mut ours: Vec<(u32, u32)> = Vec::new();
        let mut head = [0u8; HEADER];
        for index in 0..dev.count() {
            dev.read(index, 0, &mut head)?;
            if let Some((seq, theirs)) = header(&head) {
                if theirs == tag {
                    ours.push((seq, index));
                }
            }
        }
        ours.sort_unstable();
        let mut forgot = Forgot::default();
        // The frames to leave out, by sector index, by offset.
        let mut out: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        let mut buf = vec![0u8; SECTOR];
        let mut part: Option<UnderWay> = None;
        let mut previous: Option<u32> = None;
        for &(seq, index) in &ours {
            feed();
            if previous.is_some_and(|p| p.wrapping_add(1) != seq) {
                part = None;
            }
            previous = Some(seq);
            dev.read(index, 0, &mut buf)?;
            let mut at = HEADER;
            while let Next::Frame {
                flags,
                payload,
                next,
            } = next_frame(&buf, at)
            {
                let here = at;
                at = next;
                if flags & (MARK | DONE) != 0 {
                    continue;
                }
                let is_ask = flags & ASK != 0;
                let (mut whole, mut lies) = match (flags & FIRST != 0, part.take()) {
                    (true, _) => (Vec::new(), Vec::new()),
                    (false, Some((started, started_ask, lies))) if started_ask == is_ask => {
                        (started, lies)
                    }
                    (false, _) => continue,
                };
                whole.extend_from_slice(payload);
                lies.push((index, here));
                if flags & LAST == 0 {
                    part = Some((whole, is_ask, lies));
                    continue;
                }
                if is_ask {
                    continue;
                }
                if decode(&whole).is_some_and(|entry| drop(&entry)) {
                    forgot.entries += 1;
                    for (sector, offset) in lies {
                        out.entry(sector).or_default().push(offset);
                        forgot.frames += 1;
                    }
                }
            }
        }
        for (index, gone) in out {
            feed();
            dev.read(index, 0, &mut buf)?;
            let mut kept = buf[..HEADER].to_vec();
            let mut at = HEADER;
            while let Next::Frame { next, .. } = next_frame(&buf, at) {
                if !gone.contains(&at) {
                    kept.extend_from_slice(&buf[at..next]);
                }
                at = next;
            }
            dev.erase(index)?;
            dev.write(index, 0, &kept)?;
            forgot.sectors += 1;
        }
        Ok(forgot)
    }

    /// The log's frames read from `from` on, or whole when `None`.
    #[allow(clippy::type_complexity)]
    fn scan<S: Sectors>(
        dev: &mut S,
        tag: [u8; 8],
        from: Option<End>,
        clock: &dyn Fn() -> u64,
        mut each: impl FnMut(Entry, Span),
    ) -> Result<(Self, Restored, Vec<PendingAsk>, Spent, Vec<[u8; 16]>), S::Error> {
        let mut spent = Spent::default();
        let mut t = clock();
        let mut lap = |into: &mut u64| {
            let now = clock();
            *into += now.saturating_sub(t);
            t = now;
        };
        let count = dev.count();
        let mut store = Self::empty(tag, count);
        let mut restored = Restored::default();
        let mut asks: Vec<PendingAsk> = Vec::new();
        let mut done: Vec<[u8; 16]> = Vec::new();
        let mut ours: Vec<(u32, u32)> = Vec::new();
        let mut head = [0u8; HEADER];
        for index in 0..count {
            dev.read(index, 0, &mut head)?;
            let Some((seq, theirs)) = header(&head) else {
                continue;
            };
            if store.newest.is_none_or(|(_, newest)| seq > newest) {
                store.newest = Some((index, seq));
            }
            match theirs == tag {
                true => ours.push((seq, index)),
                false => restored.foreign += 1,
            }
        }
        ours.sort_unstable();
        store.used = ours.len() as u32;
        store.seqs = ours.iter().copied().collect();
        if let Some((_, seq)) = store.newest {
            store.end = (seq, 0);
        }
        let mut buf = vec![0u8; SECTOR];
        let mut part: Option<(Vec<u8>, bool, u32)> = None;
        let mut previous: Option<u32> = None;
        lap(&mut spent.read);
        for &(seq, index) in &ours {
            // The sectors a summary stands for are not read (1563).
            if from.is_some_and(|(first, _)| seq < first) {
                continue;
            }
            // A sector missing between two: an entry cannot run across it.
            if previous.is_some_and(|p| p.wrapping_add(1) != seq) {
                part = None;
            }
            previous = Some(seq);
            dev.read(index, 0, &mut buf)?;
            lap(&mut spent.read);
            restored.sectors += 1;
            // The summary's end is a frame's start: everything before it
            // in its sector is the summary's too.
            let mut at = match from {
                Some((first, offset)) if seq == first => (offset as usize).max(HEADER),
                _ => HEADER,
            };
            let mut open = false;
            loop {
                let next = next_frame(&buf, at);
                lap(&mut spent.crc);
                let (flags, payload, after) = match next {
                    Next::Open => {
                        open = true;
                        break;
                    }
                    Next::Torn => {
                        restored.torn += 1;
                        break;
                    }
                    Next::Full => break,
                    Next::Frame {
                        flags,
                        payload,
                        next,
                    } => (flags, payload, next),
                };
                at = after;
                if flags & MARK != 0 {
                    if let Ok(bytes) = <[u8; 8]>::try_from(payload) {
                        store.cursor = store.cursor.max(u64::from_le_bytes(bytes));
                    }
                    continue;
                }
                if flags & DONE != 0 {
                    if let Ok(id) = <[u8; 16]>::try_from(payload) {
                        done.push(id);
                    }
                    continue;
                }
                let is_ask = flags & ASK != 0;
                let (mut whole, from) = match (flags & FIRST != 0, part.take()) {
                    (true, _) => (Vec::new(), seq),
                    (false, Some((started, started_ask, from))) if started_ask == is_ask => {
                        (started, from)
                    }
                    // A part whose start went with an older sector, or
                    // whose start was another kind.
                    (false, _) => continue,
                };
                whole.extend_from_slice(payload);
                if flags & LAST == 0 {
                    part = Some((whole, is_ask, from));
                    continue;
                }
                if is_ask {
                    if whole.len() >= 17 {
                        let mut id = [0u8; 16];
                        id.copy_from_slice(&whole[..16]);
                        asks.push(PendingAsk {
                            id,
                            tag: whole[16],
                            request: whole[17..].to_vec(),
                        });
                    }
                    continue;
                }
                let decoded = decode(&whole);
                lap(&mut spent.decode);
                if let Some(entry) = decoded {
                    store.cursor = store.cursor.max(entry.seq);
                    store.entries += 1;
                    restored.entries += 1;
                    each(entry, (from, seq));
                    lap(&mut spent.each);
                }
            }
            if store.newest == Some((index, seq)) {
                store.end = (seq, at as u32);
                if open {
                    store.tail = Some((index, at));
                }
            }
        }
        asks.retain(|ask| !done.contains(&ask.id));
        Ok((store, restored, asks, spent, done))
    }

    fn empty(tag: [u8; 8], count: u32) -> Self {
        Self {
            tag,
            count,
            newest: None,
            tail: None,
            cursor: 0,
            entries: 0,
            used: 0,
            erases: 0,
            ready: None,
            seqs: BTreeMap::new(),
            end: (0, 0),
            opened: 0,
        }
    }

    /// Open the log of the home tagged `tag` on `dev` without reading its
    /// entries (1521): every sector's header and the newest sector, which
    /// is where the log ends ([`Store::end`]). The cursor and the entry
    /// count are the caller's to set ([`Store::resume_counts`]) from the
    /// summary that said the same end; a caller whose summary says
    /// another end opens with [`Store::open`] instead.
    pub fn resume<S: Sectors>(dev: &mut S, tag: [u8; 8]) -> Result<Self, S::Error> {
        let count = dev.count();
        let mut store = Self::empty(tag, count);
        let mut head = [0u8; HEADER];
        for index in 0..count {
            dev.read(index, 0, &mut head)?;
            let Some((seq, theirs)) = header(&head) else {
                continue;
            };
            if store.newest.is_none_or(|(_, newest)| seq > newest) {
                store.newest = Some((index, seq));
            }
            if theirs == tag {
                store.seqs.insert(seq, index);
            }
        }
        store.used = store.seqs.len() as u32;
        let Some((index, seq)) = store.newest else {
            return Ok(store);
        };
        store.end = (seq, 0);
        if store.seqs.get(&seq) != Some(&index) {
            return Ok(store);
        }
        let mut buf = vec![0u8; SECTOR];
        dev.read(index, 0, &mut buf)?;
        let mut at = HEADER;
        let mut open = false;
        while at + FRAME <= SECTOR {
            let frame = &buf[at..at + FRAME];
            if frame.iter().all(|&b| b == 0xFF) {
                open = buf[at..].iter().all(|&b| b == 0xFF);
                break;
            }
            let len = usize::from(u16::from_le_bytes([frame[0], frame[1]]));
            let end = at + FRAME + len;
            if end > SECTOR || frame[3] != 0 {
                break;
            }
            let sum = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
            if crc32_parts(&frame[..4], &buf[at + FRAME..end]) != sum {
                break;
            }
            at = at + FRAME + pad4(len);
        }
        store.end = (seq, at as u32);
        if open {
            store.tail = Some((index, at));
        }
        Ok(store)
    }

    /// The cursor and the entry count a resumed log stands at, as the
    /// summary written at its end said them.
    pub fn resume_counts(&mut self, cursor: u64, entries: u64) {
        self.cursor = cursor;
        self.entries = entries;
    }

    /// Where the log ends ([`End`]).
    pub fn end(&self) -> End {
        self.end
    }

    /// Sectors opened since boot.
    pub fn opened(&self) -> u64 {
        self.opened
    }

    /// The oldest sequence of this home's sectors still in the log.
    pub fn oldest_seq(&self) -> Option<u32> {
        self.seqs.keys().next().copied()
    }

    /// Whether the sector of sequence `seq` is still in the log.
    pub fn holds(&self, seq: u32) -> bool {
        self.seqs.contains_key(&seq)
    }

    /// Read the entries that start in the sectors of `seqs`, ascending,
    /// oldest first, each with its span (1521, a conversation's page):
    /// an entry that runs on into the next sector is read whole when that
    /// sector is the next of `seqs`. Sequences the ring took back are
    /// skipped. Marks, asks and dones are no entries.
    pub fn read_seqs<S: Sectors>(
        &self,
        dev: &mut S,
        seqs: &[u32],
        mut each: impl FnMut(Entry, Span),
    ) -> Result<(), S::Error> {
        let mut buf = vec![0u8; SECTOR];
        let mut part: Option<(Vec<u8>, u32)> = None;
        let mut previous: Option<u32> = None;
        for &seq in seqs {
            let Some(&index) = self.seqs.get(&seq) else {
                part = None;
                continue;
            };
            if previous.is_some_and(|p| p.wrapping_add(1) != seq) {
                part = None;
            }
            previous = Some(seq);
            dev.read(index, 0, &mut buf)?;
            let mut head = [0u8; HEADER];
            head.copy_from_slice(&buf[..HEADER]);
            if header(&head) != Some((seq, self.tag)) {
                part = None;
                continue;
            }
            let mut at = HEADER;
            while at + FRAME <= SECTOR {
                let frame = &buf[at..at + FRAME];
                if frame.iter().all(|&b| b == 0xFF) {
                    break;
                }
                let len = usize::from(u16::from_le_bytes([frame[0], frame[1]]));
                let flags = frame[2];
                let end = at + FRAME + len;
                if end > SECTOR || frame[3] != 0 {
                    break;
                }
                let payload = &buf[at + FRAME..end];
                let sum = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
                if crc32_parts(&frame[..4], payload) != sum {
                    break;
                }
                at = at + FRAME + pad4(len);
                if flags & SPECIAL != 0 {
                    continue;
                }
                let (mut whole, from) = match (flags & FIRST != 0, part.take()) {
                    (true, _) => (Vec::new(), seq),
                    (false, Some(started)) => started,
                    (false, None) => continue,
                };
                whole.extend_from_slice(payload);
                if flags & LAST == 0 {
                    part = Some((whole, from));
                    continue;
                }
                if let Some(entry) = decode(&whole) {
                    each(entry, (from, seq));
                }
            }
        }
        Ok(())
    }

    /// The newest sequence the store holds: where its cursor stands.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Entries held.
    pub fn entries(&self) -> u64 {
        self.entries
    }

    /// Sectors this home's entries take.
    pub fn used(&self) -> u32 {
        self.used
    }

    /// Sectors of the store.
    pub fn sectors(&self) -> u32 {
        self.count
    }

    /// Sectors erased since boot.
    pub fn erases(&self) -> u64 {
        self.erases
    }

    /// Write one page: `entries` (what the fold keeps of it) and a mark
    /// of `last`, the page's newest sequence, in one flash write per
    /// sector the page touches. The cursor is `last` once it returns.
    pub fn append<S: Sectors>(
        &mut self,
        dev: &mut S,
        entries: &[Entry],
        last: u64,
    ) -> Result<Appended, S::Error> {
        self.append_spans(dev, entries, last).map(|(done, _)| done)
    }

    /// [`Store::append`], and each entry's [`Span`] in `entries`' order
    /// (1521): what the per-conversation index takes in.
    pub fn append_spans<S: Sectors>(
        &mut self,
        dev: &mut S,
        entries: &[Entry],
        last: u64,
    ) -> Result<(Appended, Vec<Span>), S::Error> {
        let mut mark = [0u8; 8];
        mark.copy_from_slice(&last.max(self.cursor).to_le_bytes());
        let mut items: Vec<(Vec<u8>, u8)> = entries.iter().map(|e| (encode(e), 0)).collect();
        items.push((mark.to_vec(), MARK));
        let mut spans = Vec::with_capacity(items.len());
        let done = self.put_spans(dev, &items, &mut spans)?;
        spans.truncate(entries.len());
        self.entries += done.entries;
        self.cursor = self.cursor.max(last);
        Ok((done, spans))
    }

    /// Keep an ask the board owes its home (1493): `request`'s wire bytes
    /// under the verb's `tag`, so a reboot queues it again until
    /// [`Store::ask_done`] writes `id` behind it.
    pub fn keep_ask<S: Sectors>(
        &mut self,
        dev: &mut S,
        id: &[u8; 16],
        tag: u8,
        request: &[u8],
    ) -> Result<Appended, S::Error> {
        let mut bytes = Vec::with_capacity(17 + request.len());
        bytes.extend_from_slice(id);
        bytes.push(tag);
        bytes.extend_from_slice(request);
        self.put(dev, &[(bytes, ASK)])
    }

    /// The ask under `id` ended: taken by the home, or refused for good.
    /// The next boot does not queue it again.
    pub fn ask_done<S: Sectors>(
        &mut self,
        dev: &mut S,
        id: &[u8; 16],
    ) -> Result<Appended, S::Error> {
        self.put(dev, &[(id.to_vec(), DONE)])
    }

    /// Write `items` as frames, each under its base flags: 0 an entry,
    /// [`MARK`], [`ASK`], [`DONE`]. Entries and asks split across sectors;
    /// a mark and a done never do.
    fn put<S: Sectors>(
        &mut self,
        dev: &mut S,
        items: &[(Vec<u8>, u8)],
    ) -> Result<Appended, S::Error> {
        self.put_spans(dev, items, &mut Vec::new())
    }

    /// [`Store::put`], each item's [`Span`] pushed onto `spans`.
    fn put_spans<S: Sectors>(
        &mut self,
        dev: &mut S,
        items: &[(Vec<u8>, u8)],
        spans: &mut Vec<Span>,
    ) -> Result<Appended, S::Error> {
        let mut done = Appended::default();
        let here = |store: &Self| store.newest.map_or(0, |(_, seq)| seq);
        // The bytes owed the current sector from `start` on.
        let mut pending: Vec<u8> = Vec::new();
        let mut start = 0;
        for (bytes, base) in items {
            let whole_only = *base & (MARK | DONE) != 0;
            let mut rest: &[u8] = bytes;
            let mut first = true;
            let mut from = 0;
            while first || !rest.is_empty() {
                let room = match self.tail {
                    Some((_, at)) => (SECTOR - at - pending.len()).saturating_sub(FRAME),
                    None => 0,
                };
                // A mark and a done are never split; nor is a part made
                // smaller than is worth a header when the item has more
                // than that.
                if room < LEAST_PART.min(rest.len().max(1)) || (whole_only && room < rest.len()) {
                    self.flush(dev, &mut pending, start, &mut done)?;
                    self.next_sector(dev, &mut done)?;
                    start = self.tail.map_or(HEADER, |(_, at)| at);
                    continue;
                }
                if pending.is_empty() {
                    start = self.tail.map_or(HEADER, |(_, at)| at);
                }
                let take = rest.len().min(room);
                let mut flags = *base;
                if first {
                    flags |= FIRST;
                    from = here(self);
                }
                if take == rest.len() {
                    flags |= LAST;
                }
                let part = &rest[..take];
                let head = [(take & 0xFF) as u8, (take >> 8) as u8, flags, 0];
                pending.extend_from_slice(&head);
                pending.extend_from_slice(&crc32_parts(&head, part).to_le_bytes());
                pending.extend_from_slice(part);
                pending.resize(pending.len() + pad4(take) - take, 0);
                rest = &rest[take..];
                first = false;
                if flags & LAST != 0 && *base == 0 {
                    done.entries += 1;
                }
                if flags & LAST != 0 {
                    spans.push((from, here(self)));
                }
            }
        }
        self.flush(dev, &mut pending, start, &mut done)?;
        Ok(done)
    }

    /// How many sectors [`Store::append`] of `entries` would open, each
    /// one an erase unless a quiet moment erased it ahead (1597): the
    /// placement `Store::put_spans` makes, the page's mark with it,
    /// nothing written. A writer bounded in erases asks before it
    /// writes.
    pub fn opens(&self, entries: &[Entry]) -> u32 {
        let lens = entries
            .iter()
            .map(|entry| (ENTRY_HEAD + entry.packed.len(), false))
            .chain(core::iter::once((8, true)));
        let mut at = self.tail.map(|(_, at)| at);
        let mut opened = 0;
        for (len, whole_only) in lens {
            let mut rest = len;
            let mut first = true;
            while first || rest > 0 {
                let room = match at {
                    Some(at) => (SECTOR - at).saturating_sub(FRAME),
                    None => 0,
                };
                if room < LEAST_PART.min(rest.max(1)) || (whole_only && room < rest) {
                    opened += 1;
                    at = Some(HEADER);
                    continue;
                }
                let take = rest.min(room);
                at = at.map(|at| at + FRAME + pad4(take));
                rest -= take;
                first = false;
            }
        }
        opened
    }

    /// Take nothing more into the open sector (1540, the M9's bench): the
    /// next write opens the next sector, and erases it, as a full sector
    /// would. What the closed sector holds stays and reads back; its
    /// erased rest is never written. A quiet moment before the write may
    /// still erase ahead ([`Store::pre_erase`]), so the bench closes the
    /// tail right before the write it means to make erase.
    pub fn close(&mut self) {
        self.tail = None;
        self.ready = None;
    }

    /// Erase the ring's next sector while the line is quiet (1493): the
    /// page path then writes into erased flash and never holds the
    /// interrupts for an erase (esp-storage runs every flash op inside a
    /// critical section, and an erase of tens of milliseconds overflows
    /// the 128-byte UART FIFO, 11 ms at 115200). At most one erase per
    /// call; whether one was made. A call that finds the sector already
    /// erased, or already known so, costs one read.
    pub fn pre_erase<S: Sectors>(&mut self, dev: &mut S) -> Result<bool, S::Error> {
        let index = match self.newest {
            Some((index, _)) => (index + 1) % self.count,
            None => 0,
        };
        // The open tail is never erased under its own frames.
        if self.ready == Some(index) || self.tail.is_some_and(|(tail, _)| tail == index) {
            return Ok(false);
        }
        let mut buf = vec![0u8; SECTOR];
        dev.read(index, 0, &mut buf)?;
        if buf.iter().all(|&b| b == 0xFF) {
            self.ready = Some(index);
            return Ok(false);
        }
        // What the ring takes back, counted before the erase, as
        // `next_sector` counts it.
        let mut head = [0u8; HEADER];
        head.copy_from_slice(&buf[..HEADER]);
        if let Some((seq, theirs)) = header(&head) {
            if theirs == self.tag {
                let lost = count_entries(dev, index)?;
                self.entries = self.entries.saturating_sub(lost);
                self.used = self.used.saturating_sub(1);
                self.seqs.remove(&seq);
            }
        }
        dev.erase(index)?;
        self.erases += 1;
        self.ready = Some(index);
        Ok(true)
    }

    /// Write what is owed the tail sector from `start`, and move the tail.
    fn flush<S: Sectors>(
        &mut self,
        dev: &mut S,
        pending: &mut Vec<u8>,
        start: usize,
        done: &mut Appended,
    ) -> Result<(), S::Error> {
        let Some((index, _)) = self.tail else {
            pending.clear();
            return Ok(());
        };
        if pending.is_empty() {
            return Ok(());
        }
        // Until the write is known to have landed the sector is closed: a
        // failed write leaves bytes the next one must not write over.
        self.tail = None;
        dev.write(index, start, pending)?;
        self.tail = Some((index, start + pending.len()));
        if let Some((_, seq)) = self.newest {
            self.end = (seq, (start + pending.len()) as u32);
        }
        done.writes += 1;
        pending.clear();
        Ok(())
    }

    /// Erase the next sector of the ring and open it under a new header.
    fn next_sector<S: Sectors>(
        &mut self,
        dev: &mut S,
        done: &mut Appended,
    ) -> Result<(), S::Error> {
        let (index, seq) = match self.newest {
            Some((index, seq)) => ((index + 1) % self.count, seq.wrapping_add(1)),
            None => (0, 1),
        };
        // Pre-erased in a quiet moment (1493): the page path skips the
        // erase and only writes.
        let ready = self.ready == Some(index);
        if ready {
            self.ready = None;
        }
        if !ready {
            // What the ring takes back: the sector's entries, when they
            // were this home's.
            let mut head = [0u8; HEADER];
            dev.read(index, 0, &mut head)?;
            if let Some((old, theirs)) = header(&head) {
                if theirs == self.tag {
                    let lost = count_entries(dev, index)?;
                    self.entries = self.entries.saturating_sub(lost);
                    self.used = self.used.saturating_sub(1);
                    self.seqs.remove(&old);
                }
            }
        }
        self.tail = None;
        if !ready {
            dev.erase(index)?;
            self.erases += 1;
            done.erased += 1;
        }
        self.newest = Some((index, seq));
        let mut head = [0u8; HEADER];
        head[..4].copy_from_slice(MAGIC);
        head[4] = VERSION;
        head[8..12].copy_from_slice(&seq.to_le_bytes());
        head[12..20].copy_from_slice(&self.tag);
        let sum = crc32_parts(&head[..20], &[]);
        head[20..24].copy_from_slice(&sum.to_le_bytes());
        // The sector the ring took back by the pre-erase may have been
        // this home's: its sequence goes with it.
        self.seqs.retain(|_, &mut at| at != index);
        dev.write(index, 0, &head)?;
        self.used += 1;
        self.seqs.insert(seq, index);
        self.end = (seq, HEADER as u32);
        self.opened += 1;
        self.tail = Some((index, HEADER));
        Ok(())
    }
}

/// The sequence and tag of a header that checks.
fn header(head: &[u8; HEADER]) -> Option<(u32, [u8; 8])> {
    if &head[..4] != MAGIC || head[4] != VERSION {
        return None;
    }
    let sum = u32::from_le_bytes([head[20], head[21], head[22], head[23]]);
    if crc32_parts(&head[..20], &[]) != sum {
        return None;
    }
    let seq = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
    let mut tag = [0u8; 8];
    tag.copy_from_slice(&head[12..20]);
    Some((seq, tag))
}

/// An entry [`Store::forget`] has begun to read: its bytes so far,
/// whether an ask, and where its frames lie (sector index, offset).
type UnderWay = (Vec<u8>, bool, Vec<(u32, usize)>);

/// What lies at `at` of a sector's bytes `buf` (1562, shared by the
/// boot's scan and [`Store::forget`]).
enum Next<'a> {
    /// Erased to the sector's end: where the next frame goes.
    Open,
    /// No room for another frame header.
    Full,
    /// Bytes that are no frame that checks: the end of a cut write.
    Torn,
    /// A frame that checks: its flags, its payload and where the frame
    /// after it starts.
    Frame {
        flags: u8,
        payload: &'a [u8],
        next: usize,
    },
}

fn next_frame(buf: &[u8], at: usize) -> Next<'_> {
    let Some(frame) = buf.get(at..at + FRAME) else {
        return Next::Full;
    };
    if frame.iter().all(|&b| b == 0xFF) {
        return match buf[at..].iter().all(|&b| b == 0xFF) {
            true => Next::Open,
            false => Next::Torn,
        };
    }
    let len = usize::from(u16::from_le_bytes([frame[0], frame[1]]));
    let end = at + FRAME + len;
    if end > buf.len() || frame[3] != 0 {
        return Next::Torn;
    }
    let payload = &buf[at + FRAME..end];
    let sum = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
    if crc32_parts(&frame[..4], payload) != sum {
        return Next::Torn;
    }
    Next::Frame {
        flags: frame[2],
        payload,
        next: (at + FRAME + pad4(len)).min(buf.len()),
    }
}

/// The entries that start in sector `index`: what erasing it costs (one
/// that runs on into the next sector cannot be read without its start).
fn count_entries<S: Sectors>(dev: &mut S, index: u32) -> Result<u64, S::Error> {
    let mut buf = vec![0u8; SECTOR];
    dev.read(index, 0, &mut buf)?;
    let mut at = HEADER;
    let mut n = 0;
    while at + FRAME <= SECTOR {
        let frame = &buf[at..at + FRAME];
        if frame.iter().all(|&b| b == 0xFF) {
            break;
        }
        let len = usize::from(u16::from_le_bytes([frame[0], frame[1]]));
        if at + FRAME + len > SECTOR {
            break;
        }
        if frame[2] & SPECIAL == 0 && frame[2] & FIRST != 0 {
            n += 1;
        }
        at += FRAME + pad4(len);
    }
    Ok(n)
}

fn encode(entry: &Entry) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENTRY_HEAD + entry.packed.len());
    out.extend_from_slice(&entry.seq.to_le_bytes());
    out.push(entry.kind);
    out.extend_from_slice(&entry.at.to_le_bytes());
    out.extend_from_slice(&entry.packed);
    out
}

fn decode(bytes: &[u8]) -> Option<Entry> {
    if bytes.len() < ENTRY_HEAD {
        return None;
    }
    let seq = u64::from_le_bytes(bytes[..8].try_into().ok()?);
    let at = i64::from_le_bytes(bytes[9..17].try_into().ok()?);
    Some(Entry {
        seq,
        kind: bytes[8],
        at,
        packed: bytes[ENTRY_HEAD..].to_vec(),
    })
}

/// The bytes `entry` takes in a store, its frame's header counted, when
/// it fits one frame (1597, [`super::rows::WRITE_BUDGET`]).
pub fn framed_len(entry: &Entry) -> usize {
    FRAME + pad4(ENTRY_HEAD + entry.packed.len())
}

fn pad4(len: usize) -> usize {
    (len + 3) & !3
}

/// The CRC-32 of IEEE 802.3 (the zlib one) over `a` then `b`.
fn crc32_parts(a: &[u8], b: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in a.iter().chain(b) {
        crc = CRC_TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// A flash in memory with the part's rules: a write into bytes that are
/// not erased is refused, as it would corrupt them on the part.
#[cfg(any(test, feature = "std"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemFlash {
    pub sectors: Vec<Vec<u8>>,
    /// Writes and erases made, for the tests' counts.
    pub writes: u64,
    pub erases: u64,
    /// When set, the write with this number (counted from 1) lands only
    /// its first half and then fails: a power loss mid-write.
    pub cut_at: Option<u64>,
}

#[cfg(any(test, feature = "std"))]
impl MemFlash {
    /// `count` erased sectors.
    pub fn new(count: u32) -> Self {
        Self {
            sectors: vec![vec![0xFF; SECTOR]; count as usize],
            writes: 0,
            erases: 0,
            cut_at: None,
        }
    }
}

/// What the memory flash refuses.
#[cfg(any(test, feature = "std"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    /// Off the sector or off a word.
    Bounds,
    /// A byte written that was not erased.
    NotErased,
    /// The power went.
    Cut,
}

#[cfg(any(test, feature = "std"))]
impl Sectors for MemFlash {
    type Error = MemError;

    fn count(&self) -> u32 {
        self.sectors.len() as u32
    }

    fn read(&mut self, sector: u32, offset: usize, buf: &mut [u8]) -> Result<(), MemError> {
        let s = self.sectors.get(sector as usize).ok_or(MemError::Bounds)?;
        let src = s.get(offset..offset + buf.len()).ok_or(MemError::Bounds)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write(&mut self, sector: u32, offset: usize, data: &[u8]) -> Result<(), MemError> {
        if !offset.is_multiple_of(4) || !data.len().is_multiple_of(4) {
            return Err(MemError::Bounds);
        }
        self.writes += 1;
        let cut = self.cut_at == Some(self.writes);
        let s = self
            .sectors
            .get_mut(sector as usize)
            .ok_or(MemError::Bounds)?;
        let dst = s
            .get_mut(offset..offset + data.len())
            .ok_or(MemError::Bounds)?;
        if dst.iter().any(|&b| b != 0xFF) {
            return Err(MemError::NotErased);
        }
        let n = if cut { data.len() / 2 } else { data.len() };
        dst[..n].copy_from_slice(&data[..n]);
        match cut {
            true => Err(MemError::Cut),
            false => Ok(()),
        }
    }

    fn erase(&mut self, sector: u32) -> Result<(), MemError> {
        let s = self
            .sectors
            .get_mut(sector as usize)
            .ok_or(MemError::Bounds)?;
        s.fill(0xFF);
        self.erases += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
