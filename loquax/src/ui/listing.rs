//! A conversation's row as the store keeps it (1490, schema 17): what
//! the chats list needs with no message loaded. Written by the fold
//! that changed the conversation ([`super::UiModel::take_writes`]
//! derives it with [`super::UiModel::listing_for`]), read back at start
//! (`Store::start`), so a restart reads 300 rows instead of unpacking
//! 4000 bodies; the bodies load when their conversation opens, through
//! the same page reads the scrollback uses.
//!
//! It is data, never the truth about loaded messages: a conversation
//! whose messages are in memory derives its row from them as before,
//! and the listing is only consulted while `messages` is empty with
//! more in the store. A position row's words are not persisted: its
//! liveness is re-judged from [`Listing::report_times`] at the clock
//! the screen shows, so no stored age ever goes stale.
//!
//! The encoding is a small versioned byte form of our own
//! ([`Listing::encode`]), shared by the SQLite store's BLOB column and
//! the M9's flash frames: `no_std`, explicit, and a decode failure is
//! `None`, which every reader treats as "walk the messages once more".

use alloc::string::String;
use alloc::vec::Vec;

use super::chat::Mark;
use crate::when::WallTime;

/// How many report times the listing keeps for the liveness judgement:
/// the cadence's gaps and one more, as the summary keeps indices.
pub const REPORT_TIMES: usize = super::liveness::CADENCE_GAPS + 1;

/// What a conversation's last message is, as the row words it
/// ([`super::chat::kind`]), as one byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LastKind {
    #[default]
    Text,
    Position,
    Reaction,
    Photo,
    File,
    Voice,
}

impl LastKind {
    /// The row's word, [`super::chat::kind`]'s.
    pub fn word(self) -> &'static str {
        match self {
            LastKind::Text => "text",
            LastKind::Position => "position",
            LastKind::Reaction => "reaction",
            LastKind::Photo => "photo",
            LastKind::File => "file",
            LastKind::Voice => "voice",
        }
    }

    pub fn of_word(word: &str) -> Self {
        match word {
            "position" => LastKind::Position,
            "reaction" => LastKind::Reaction,
            "photo" => LastKind::Photo,
            "file" => LastKind::File,
            "voice" => LastKind::Voice,
            _ => LastKind::Text,
        }
    }

    fn code(self) -> u8 {
        match self {
            LastKind::Text => 0,
            LastKind::Position => 1,
            LastKind::Reaction => 2,
            LastKind::Photo => 3,
            LastKind::File => 4,
            LastKind::Voice => 5,
        }
    }

    fn of_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => LastKind::Text,
            1 => LastKind::Position,
            2 => LastKind::Reaction,
            3 => LastKind::Photo,
            4 => LastKind::File,
            5 => LastKind::Voice,
            _ => return None,
        })
    }
}

/// A conversation's persisted row (schema 17).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// [`super::Conversation::last_written`]'s answer: the list's order
    /// key.
    pub moved: Option<i64>,
    /// The last line, rendered as the list renders it; recomputed from
    /// the liveness for a position row.
    pub line: String,
    /// The last message was ours.
    pub ours: bool,
    /// When it was written.
    pub at: Option<WallTime>,
    /// What it is.
    pub kind: LastKind,
    /// A reaction's content, when it is one.
    pub reaction: Option<String>,
    /// Our last message's delivery mark.
    pub mark: Option<Mark>,
    /// When the peer read it, for the tick's words.
    pub read_at: Option<u32>,
    /// The unread rows the badge counts.
    pub unread_rows: u32,
    /// The report run's length when the last row stands for one.
    pub updates: u32,
    /// The avatar's announced appearance, foreground and background.
    pub appearance: Option<(String, String)>,
    /// The newest reports' order times, newest first, at most
    /// [`REPORT_TIMES`]: what the liveness is judged from.
    pub report_times: Vec<i64>,
    /// The newest report's share mark.
    pub share_every: Option<u32>,
    pub share_ended: bool,
}

const VERSION: u8 = 1;

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_opt_i64(out: &mut Vec<u8>, value: Option<i64>) {
    match value {
        Some(value) => {
            out.push(1);
            put_i64(out, value);
        }
        None => out.push(0),
    }
}

fn put_str(out: &mut Vec<u8>, value: &str) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

struct Take<'a>(&'a [u8]);

impl Take<'_> {
    fn u8(&mut self) -> Option<u8> {
        let (&first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(first)
    }

    fn u32(&mut self) -> Option<u32> {
        let (bytes, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(u32::from_le_bytes(*bytes))
    }

    fn i64(&mut self) -> Option<i64> {
        let (bytes, rest) = self.0.split_first_chunk::<8>()?;
        self.0 = rest;
        Some(i64::from_le_bytes(*bytes))
    }

    fn opt_i64(&mut self) -> Option<Option<i64>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.i64()?)),
            _ => None,
        }
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        if self.0.len() < len {
            return None;
        }
        let (bytes, rest) = self.0.split_at(len);
        self.0 = rest;
        String::from_utf8(bytes.to_vec()).ok()
    }
}

fn mark_code(mark: Mark) -> u8 {
    match mark {
        Mark::Sending => 0,
        Mark::Unconfirmed => 1,
        Mark::Mailbox => 2,
        Mark::Arrived => 3,
        Mark::Delivered => 4,
        Mark::Read => 5,
        Mark::Failed => 6,
    }
}

fn mark_of(code: u8) -> Option<Mark> {
    Some(match code {
        0 => Mark::Sending,
        1 => Mark::Unconfirmed,
        2 => Mark::Mailbox,
        3 => Mark::Arrived,
        4 => Mark::Delivered,
        5 => Mark::Read,
        6 => Mark::Failed,
        _ => return None,
    })
}

impl Listing {
    /// The listing as bytes, for the store's column or a flash frame.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.line.len());
        out.push(VERSION);
        put_opt_i64(&mut out, self.moved);
        put_str(&mut out, &self.line);
        out.push(u8::from(self.ours));
        match self.at {
            Some(at) => {
                out.push(1);
                put_i64(&mut out, at.unix_secs);
                put_u32(&mut out, at.offset_secs as u32);
            }
            None => out.push(0),
        }
        out.push(self.kind.code());
        match &self.reaction {
            Some(content) => {
                out.push(1);
                put_str(&mut out, content);
            }
            None => out.push(0),
        }
        match self.mark {
            Some(mark) => {
                out.push(1);
                out.push(mark_code(mark));
            }
            None => out.push(0),
        }
        match self.read_at {
            Some(at) => {
                out.push(1);
                put_u32(&mut out, at);
            }
            None => out.push(0),
        }
        put_u32(&mut out, self.unread_rows);
        put_u32(&mut out, self.updates);
        match &self.appearance {
            Some((fg, bg)) => {
                out.push(1);
                put_str(&mut out, fg);
                put_str(&mut out, bg);
            }
            None => out.push(0),
        }
        out.push(self.report_times.len().min(REPORT_TIMES) as u8);
        for &time in self.report_times.iter().take(REPORT_TIMES) {
            put_i64(&mut out, time);
        }
        match self.share_every {
            Some(every) => {
                out.push(1);
                put_u32(&mut out, every);
            }
            None => out.push(0),
        }
        out.push(u8::from(self.share_ended));
        out
    }

    /// The listing the bytes carry; `None` for any form this build does
    /// not know, which every reader treats as "derive once more".
    pub fn decode(bytes: &[u8]) -> Option<Listing> {
        let mut take = Take(bytes);
        if take.u8()? != VERSION {
            return None;
        }
        let moved = take.opt_i64()?;
        let line = take.string()?;
        let ours = take.u8()? == 1;
        let at = match take.u8()? {
            0 => None,
            1 => {
                let unix_secs = take.i64()?;
                let offset_secs = take.u32()? as i32;
                Some(WallTime {
                    unix_secs,
                    offset_secs,
                })
            }
            _ => return None,
        };
        let kind = LastKind::of_code(take.u8()?)?;
        let reaction = match take.u8()? {
            0 => None,
            1 => Some(take.string()?),
            _ => return None,
        };
        let mark = match take.u8()? {
            0 => None,
            1 => Some(mark_of(take.u8()?)?),
            _ => return None,
        };
        let read_at = match take.u8()? {
            0 => None,
            1 => Some(take.u32()?),
            _ => return None,
        };
        let unread_rows = take.u32()?;
        let updates = take.u32()?;
        let appearance = match take.u8()? {
            0 => None,
            1 => Some((take.string()?, take.string()?)),
            _ => return None,
        };
        let count = take.u8()? as usize;
        if count > REPORT_TIMES {
            return None;
        }
        let mut report_times = Vec::with_capacity(count);
        for _ in 0..count {
            report_times.push(take.i64()?);
        }
        let share_every = match take.u8()? {
            0 => None,
            1 => Some(take.u32()?),
            _ => return None,
        };
        let share_ended = take.u8()? == 1;
        Some(Listing {
            moved,
            line,
            ours,
            at,
            kind,
            reaction,
            mark,
            read_at,
            unread_rows,
            updates,
            appearance,
            report_times,
            share_every,
            share_ended,
        })
    }

    /// The liveness the listing's reports say, judged at `now`.
    pub fn liveness(&self, now: Option<i64>) -> super::liveness::Liveness {
        let mark = leviculum_lxmf::telemetry::ShareMark {
            every: self.share_every,
            ended: self.share_ended,
        };
        super::liveness::Liveness::of_marked(&self.report_times, mark, now)
    }
}

/// The most unread message ids a [`Row`] carries: a read mark naming
/// one of them clears the badge of a conversation nothing of which is
/// loaded (1521). Older unread are cleared by any newer one's mark.
pub const UNREAD_IDS: usize = 64;

/// A conversation as a device keeps it to list it at boot without
/// reading a message (1521): the board's flash store writes one per
/// change and the boot restores the list from them alone
/// ([`super::UiModel::restore_rows`]); the messages load when the
/// conversation opens, through the same page read the scrollback uses
/// ([`super::history::StoreRead::Earlier`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// The conversation's peer.
    pub peer: [u8; 16],
    /// [`super::Conversation::unread`].
    pub unread: u32,
    /// [`super::Conversation::activity`]: the order's tie-break among
    /// equal times, as the model had it.
    pub activity: u64,
    /// The newest unread received ids, oldest first, at most
    /// [`UNREAD_IDS`].
    pub unread_ids: Vec<[u8; 32]>,
    /// The list's row: order key, last written, the preview line, the
    /// mark.
    pub listing: Listing,
}

/// A [`Row`]'s form, its own version byte ahead of the listing's form.
const ROW_VERSION: u8 = 1;

impl Row {
    /// The row as bytes.
    pub fn encode(&self) -> Vec<u8> {
        let listing = self.listing.encode();
        let ids = self.unread_ids.len().min(UNREAD_IDS);
        let mut out = Vec::with_capacity(34 + 32 * ids + listing.len());
        out.push(ROW_VERSION);
        out.extend_from_slice(&self.peer);
        put_u32(&mut out, self.unread);
        out.extend_from_slice(&self.activity.to_le_bytes());
        out.push(ids as u8);
        for id in &self.unread_ids[self.unread_ids.len() - ids..] {
            out.extend_from_slice(id);
        }
        out.extend_from_slice(&listing);
        out
    }

    /// The row the bytes carry; `None` for damage or a form this build
    /// does not know.
    pub fn decode(bytes: &[u8]) -> Option<Row> {
        let mut take = Take(bytes);
        if take.u8()? != ROW_VERSION {
            return None;
        }
        let (peer, rest) = take.0.split_first_chunk::<16>()?;
        take.0 = rest;
        let unread = take.u32()?;
        let (activity, rest) = take.0.split_first_chunk::<8>()?;
        take.0 = rest;
        let count = usize::from(take.u8()?);
        if count > UNREAD_IDS {
            return None;
        }
        let mut unread_ids = Vec::with_capacity(count);
        for _ in 0..count {
            let (id, rest) = take.0.split_first_chunk::<32>()?;
            take.0 = rest;
            unread_ids.push(*id);
        }
        Some(Row {
            peer: *peer,
            unread,
            activity: u64::from_le_bytes(*activity),
            unread_ids,
            listing: Listing::decode(take.0)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::vec;

    use super::*;

    fn full() -> Listing {
        Listing {
            moved: Some(1_793_900_000),
            line: "bis morgen! 🙂".to_owned(),
            ours: true,
            at: Some(WallTime {
                unix_secs: 1_793_899_000,
                offset_secs: 3_600,
            }),
            kind: LastKind::Voice,
            reaction: Some("👍".to_owned()),
            mark: Some(Mark::Read),
            read_at: Some(1_793_899_500),
            unread_rows: 7,
            updates: 3,
            appearance: Some(("#fff".to_owned(), "#136".to_owned())),
            report_times: vec![1_793_899_000, 1_793_897_200, 1_793_895_400],
            share_every: Some(1_800),
            share_ended: false,
        }
    }

    /// Every field round trips, full and empty.
    #[test]
    fn a_listing_round_trips() {
        for listing in [full(), Listing::default()] {
            let bytes = listing.encode();
            assert_eq!(Listing::decode(&bytes), Some(listing));
        }
    }

    /// A row round trips, and every truncation of it is none.
    #[test]
    fn a_row_round_trips() {
        let row = Row {
            peer: [7; 16],
            unread: 3,
            activity: 4_067,
            unread_ids: vec![[1; 32], [2; 32], [3; 32]],
            listing: full(),
        };
        let bytes = row.encode();
        assert_eq!(Row::decode(&bytes), Some(row));
        for cut in 0..bytes.len() {
            assert_eq!(Row::decode(&bytes[..cut]), None, "cut at {cut}");
        }
    }

    /// Damage decodes to nothing, never to a wrong row: every truncation
    /// of the full form, and a version from the future.
    #[test]
    fn a_damaged_listing_is_none() {
        let bytes = full().encode();
        for cut in 0..bytes.len() {
            let decoded = Listing::decode(&bytes[..cut]);
            assert_eq!(decoded, None, "cut at {cut}");
        }
        let mut wrong = bytes.clone();
        wrong[0] = VERSION + 1;
        assert_eq!(Listing::decode(&wrong), None);
    }
}
