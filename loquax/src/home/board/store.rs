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
//!      2    1  flags: 1 an entry's first part, 2 its last, 4 a cursor mark
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
//! A power loss leaves at most a frame that does not check: the scan
//! stops at it, the sector is closed, and the next page opens a fresh
//! sector. A sector whose header does not check is not read. Sectors of
//! another home's tag are not shown and are erased in their turn.

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
}

/// The tag of the home whose LXMF address is `address`.
pub fn tag(address: &[u8; 16]) -> [u8; 8] {
    let mut tag = [0u8; 8];
    tag.copy_from_slice(&address[..8]);
    tag
}

impl Store {
    /// Open the store of the home tagged `tag` on `dev`, and hand every
    /// entry it holds to `each`, oldest first.
    pub fn open<S: Sectors>(
        dev: &mut S,
        tag: [u8; 8],
        mut each: impl FnMut(Entry),
    ) -> Result<(Self, Restored), S::Error> {
        let count = dev.count();
        let mut store = Self {
            tag,
            count,
            newest: None,
            tail: None,
            cursor: 0,
            entries: 0,
            used: 0,
            erases: 0,
        };
        let mut restored = Restored::default();
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
        let mut buf = vec![0u8; SECTOR];
        let mut part: Option<Vec<u8>> = None;
        let mut previous: Option<u32> = None;
        for &(seq, index) in &ours {
            // A sector missing between two: an entry cannot run across it.
            if previous.is_some_and(|p| p.wrapping_add(1) != seq) {
                part = None;
            }
            previous = Some(seq);
            dev.read(index, 0, &mut buf)?;
            restored.sectors += 1;
            let mut at = HEADER;
            let mut open = false;
            while at + FRAME <= SECTOR {
                let frame = &buf[at..at + FRAME];
                if frame.iter().all(|&b| b == 0xFF) {
                    open = buf[at..].iter().all(|&b| b == 0xFF);
                    if !open {
                        restored.torn += 1;
                    }
                    break;
                }
                let len = usize::from(u16::from_le_bytes([frame[0], frame[1]]));
                let flags = frame[2];
                let end = at + FRAME + len;
                if end > SECTOR || frame[3] != 0 {
                    restored.torn += 1;
                    break;
                }
                let payload = &buf[at + FRAME..end];
                let sum = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
                if crc32_parts(&frame[..4], payload) != sum {
                    restored.torn += 1;
                    break;
                }
                at = at + FRAME + pad4(len);
                if flags & MARK != 0 {
                    if let Ok(bytes) = <[u8; 8]>::try_from(payload) {
                        store.cursor = store.cursor.max(u64::from_le_bytes(bytes));
                    }
                    continue;
                }
                let mut whole = match (flags & FIRST != 0, part.take()) {
                    (true, _) => Vec::new(),
                    (false, Some(started)) => started,
                    // A part whose start went with an older sector.
                    (false, None) => continue,
                };
                whole.extend_from_slice(payload);
                if flags & LAST == 0 {
                    part = Some(whole);
                    continue;
                }
                if let Some(entry) = decode(&whole) {
                    store.cursor = store.cursor.max(entry.seq);
                    store.entries += 1;
                    restored.entries += 1;
                    each(entry);
                }
            }
            if open && store.newest == Some((index, seq)) {
                store.tail = Some((index, at));
            }
        }
        Ok((store, restored))
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
        let mut done = Appended::default();
        // The bytes owed the current sector from `start` on.
        let mut pending: Vec<u8> = Vec::new();
        let mut start = 0;
        let mut mark = [0u8; 8];
        mark.copy_from_slice(&last.max(self.cursor).to_le_bytes());
        let mut items: Vec<(Vec<u8>, bool)> = entries.iter().map(|e| (encode(e), false)).collect();
        items.push((mark.to_vec(), true));
        for (bytes, is_mark) in &items {
            let mut rest: &[u8] = bytes;
            let mut first = true;
            while first || !rest.is_empty() {
                let room = match self.tail {
                    Some((_, at)) => (SECTOR - at - pending.len()).saturating_sub(FRAME),
                    None => 0,
                };
                // A mark is never split; nor is a part made smaller than
                // is worth a header when the entry has more than that.
                if room < LEAST_PART.min(rest.len().max(1)) || (*is_mark && room < rest.len()) {
                    self.flush(dev, &mut pending, start, &mut done)?;
                    self.next_sector(dev, &mut done)?;
                    start = self.tail.map_or(HEADER, |(_, at)| at);
                    continue;
                }
                if pending.is_empty() {
                    start = self.tail.map_or(HEADER, |(_, at)| at);
                }
                let take = rest.len().min(room);
                let mut flags = if *is_mark { MARK } else { 0 };
                if first {
                    flags |= FIRST;
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
                if flags & LAST != 0 && !*is_mark {
                    done.entries += 1;
                }
            }
        }
        self.flush(dev, &mut pending, start, &mut done)?;
        self.entries += done.entries;
        self.cursor = self.cursor.max(last);
        Ok(done)
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
        // What the ring takes back: the sector's entries, when they were
        // this home's.
        let mut head = [0u8; HEADER];
        dev.read(index, 0, &mut head)?;
        if header(&head).is_some_and(|(_, theirs)| theirs == self.tag) {
            let lost = count_entries(dev, index)?;
            self.entries = self.entries.saturating_sub(lost);
            self.used = self.used.saturating_sub(1);
        }
        self.tail = None;
        dev.erase(index)?;
        self.erases += 1;
        done.erased += 1;
        self.newest = Some((index, seq));
        let mut head = [0u8; HEADER];
        head[..4].copy_from_slice(MAGIC);
        head[4] = VERSION;
        head[8..12].copy_from_slice(&seq.to_le_bytes());
        head[12..20].copy_from_slice(&self.tag);
        let sum = crc32_parts(&head[..20], &[]);
        head[20..24].copy_from_slice(&sum.to_le_bytes());
        dev.write(index, 0, &head)?;
        self.used += 1;
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
        if frame[2] & MARK == 0 && frame[2] & FIRST != 0 {
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
