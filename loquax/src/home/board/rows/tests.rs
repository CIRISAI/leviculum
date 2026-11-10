//! The summary log on its own (1521): its records replay, a snapshot
//! keeps every conversation's newest record while the log's ring comes
//! round many times, and the index files entries by conversation.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::home::board::store::MemFlash;
use crate::ui::listing::Listing;

const TAG: [u8; 8] = *b"homehome";

fn row(peer: u8, line: &str) -> Row {
    Row {
        peer: [peer; 16],
        unread: u32::from(peer),
        activity: u64::from(peer),
        unread_ids: vec![[peer; 32]],
        listing: Listing {
            line: String::from(line),
            moved: Some(i64::from(peer)),
            ..Listing::default()
        },
    }
}

fn state(cursor: u64) -> State {
    State {
        end: (3, 96),
        cursor,
        entries: 7,
        marks: vec![1, 2],
        asks: vec![PendingAsk {
            id: [9; 16],
            tag: 2,
            request: vec![1, 2, 3],
        }],
        placed: true,
    }
}

/// Owing everything again (1540, the M9 bench's owe): a written index
/// owes nothing, `owe_all` owes every row and name, the write carries
/// them, and a closed log's write opens its next sector with an erase.
#[test]
fn owe_all_writes_every_row_again_and_a_closed_log_erases() {
    let mut flash = MemFlash::new(16);
    let (mut log, _) = RowLog::open(&mut flash, TAG).expect("open");
    let mut index = Index::default();
    index.row(row(1, "one"));
    index.row(row(2, "two"));
    index.names.insert([1; 16], String::from("Anna"));
    log.write(&mut flash, &mut index, Some(&state(5)))
        .expect("write");
    assert!(!index.owes());
    let done = log.write(&mut flash, &mut index, None).expect("nothing");
    assert_eq!(done, Wrote::Whole(Appended::default()));
    index.owe_all();
    assert!(index.owes());
    log.close();
    let done = log
        .write(&mut flash, &mut index, None)
        .expect("again")
        .appended();
    assert_eq!((done.entries, done.writes, done.erased), (3, 1, 1));
    assert!(!index.owes());
    let (_, replayed) = RowLog::open(&mut flash, TAG).expect("reopen");
    assert_eq!(replayed.index.kept.len(), 2);
    assert_eq!(replayed.state, Some(state(5)));
}

/// Rows, a gone conversation, a name and the state come back as written,
/// the newest record of a peer winning.
#[test]
fn records_replay_newest_first_wins() {
    let mut flash = MemFlash::new(16);
    let (mut log, replayed) = RowLog::open(&mut flash, TAG).expect("open");
    assert!(replayed.state.is_none());
    let mut index = Index::default();
    index.row(row(1, "old"));
    index.row(row(2, "two"));
    index.names.insert([1; 16], String::from("Anna"));
    index.names_owed.insert([1; 16]);
    log.write(&mut flash, &mut index, Some(&state(5)))
        .expect("write");
    index.row(row(1, "new"));
    index.gone([2; 16]);
    log.write(&mut flash, &mut index, Some(&state(6)))
        .expect("write");

    let (_, replayed) = RowLog::open(&mut flash, TAG).expect("reopen");
    let index = replayed.index;
    assert_eq!(index.kept.len(), 1, "the gone conversation stays gone");
    let kept = &index.kept[&[1; 16]];
    assert_eq!(
        kept.row.as_ref().map(|r| r.listing.line.as_str()),
        Some("new")
    );
    assert_eq!(index.names[&[1; 16]], "Anna");
    assert_eq!(replayed.state, Some(state(6)));
}

/// A log whose ring comes round many times still holds every
/// conversation's newest record: the snapshots write them again before
/// the ring takes their sectors. The positive control is the same
/// writes with snapshots never made, which loses the quiet ones.
#[test]
fn snapshots_keep_every_record_across_the_ring() {
    for snapshots in [true, false] {
        let mut flash = MemFlash::new(16);
        let (mut log, _) = RowLog::open(&mut flash, TAG).expect("open");
        let mut index = Index::default();
        for peer in 1..=40u8 {
            index.row(row(peer, "quiet"));
        }
        log.write(&mut flash, &mut index, Some(&state(1)))
            .expect("first");
        if snapshots {
            while log.snapping() {
                log.write(&mut flash, &mut index, None).expect("snapshot");
            }
        }
        // One busy conversation, written far more than the log holds.
        let line = "x".repeat(400);
        for turn in 0..400u64 {
            index.row(row(41, &line));
            index.kept.get_mut(&[41; 16]).expect("busy").entries = turn as u32;
            index.owed.insert([41; 16]);
            if snapshots {
                // Each write begins a snapshot when one is due and writes
                // it ahead of the busy row (1597).
                log.write(&mut flash, &mut index, Some(&state(turn)))
                    .expect("write");
            } else {
                let mut records = index.owed_records();
                records.push(record(STATE, 0, state(turn).encode()));
                let cursor = log.store.cursor();
                log.store
                    .append(&mut flash, &records, cursor)
                    .expect("append");
            }
        }
        let (_, replayed) = RowLog::open(&mut flash, TAG).expect("reopen");
        let quiet = (1..=40u8)
            .filter(|p| replayed.index.kept.contains_key(&[*p; 16]))
            .count();
        match snapshots {
            true => {
                assert_eq!(quiet, 40, "every quiet conversation survives");
                assert_eq!(replayed.state.map(|s| s.cursor), Some(399));
                assert_eq!(replayed.index.kept[&[41; 16]].entries, 399);
            }
            false => assert!(quiet < 40, "the control loses what nobody rewrote"),
        }
    }
}

/// 1594, window 19: a busy log written only by bounded writes (a pull,
/// no quiet moment) reached three quarters of its ring since the last
/// snapshot and wrote the next one whole in one call. A bounded write
/// now begins the snapshot when it is due and writes it a budget at a
/// time: no call erases more than a budget's sectors, and the ring still
/// keeps every quiet conversation's record.
#[test]
fn bounded_writes_write_a_due_snapshot_a_budget_at_a_time() {
    let mut flash = MemFlash::new(64);
    let (mut log, _) = RowLog::open(&mut flash, TAG).expect("open");
    let mut index = Index::default();
    // About seven sectors of quiet rows, written once.
    let quiet_line = "q".repeat(400);
    for peer in 1..=60u8 {
        index.row(row(peer, &quiet_line));
    }
    while index.owes() {
        log.write(&mut flash, &mut index, Some(&state(0)))
            .expect("first");
    }
    let line = "x".repeat(400);
    let mut most = 0;
    for turn in 0..1200u64 {
        index.row(row(200, &line));
        index.kept.get_mut(&[200; 16]).expect("busy").entries = turn as u32;
        index.owed.insert([200; 16]);
        let done = log
            .write(&mut flash, &mut index, Some(&state(turn)))
            .expect("write")
            .appended();
        most = most.max(done.erased);
    }
    assert!(most <= 3, "a bounded write erased {most} sectors");
    let (_, replayed) = RowLog::open(&mut flash, TAG).expect("reopen");
    let quiet = (1..=60u8)
        .filter(|p| replayed.index.kept.contains_key(&[*p; 16]))
        .count();
    assert_eq!(quiet, 60, "every quiet conversation survives the ring");
    assert_eq!(replayed.index.kept[&[200; 16]].entries, 1199);
}

/// 1595: the board's summary log (128 sectors, the last eighth of the
/// 1024-sector partition) holding window 20's index, 473 records in about
/// 31 sectors, written by the quiet turns' bounded writes while a pull
/// re-owes rows. A snapshot begun at half the log must be written a
/// budget at a time to its end: before, it was late at three quarters
/// (64 + 31 sectors and the pull's own records crossed 96) and the
/// write that found it late wrote its rest at once, 243 to 473 records,
/// up to 31 writes and 30 erases in one call, the 1 to 1.7 s takes of
/// window 20's hang.walk. No bounded call may write more than the budget
/// and the sectors it crosses into, and every row survives the ring.
#[test]
fn a_snapshot_of_the_boards_index_is_written_a_budget_at_a_time() {
    let mut flash = MemFlash::new(128);
    let (mut log, _) = RowLog::open(&mut flash, TAG).expect("open");
    let mut index = Index::default();
    // Window 20's: 473 records of about 268 B, 31 sectors written whole.
    let line = "r".repeat(140);
    let peers = 473u32;
    let peer = |n: u32| {
        let mut p = [0u8; 16];
        p[..4].copy_from_slice(&n.to_le_bytes());
        p
    };
    for n in 0..peers {
        let mut r = row(1, &line);
        r.peer = peer(n);
        index.row(r);
    }
    let first = log.used();
    while index.owes() || log.snapping() {
        log.write(&mut flash, &mut index, Some(&state(0)))
            .expect("first");
    }
    let took = log.used() - first;
    assert!(
        (25..=40).contains(&took),
        "the whole index takes about window 20's 31 sectors, took {took}"
    );
    let mut most_erased = 0;
    let mut most_entries = 0;
    let mut snapshots = 0;
    for turn in 0..4000u64 {
        // A pull's page: ten rows said again.
        for k in 0..10u64 {
            let n = ((turn * 10 + k) % u64::from(peers)) as u32;
            let mut r = row(1, &line);
            r.peer = peer(n);
            r.unread = turn as u32;
            index.row(r);
        }
        let snapping = log.snapping();
        let done = log
            .write(&mut flash, &mut index, Some(&state(turn)))
            .expect("write")
            .appended();
        if snapping && !log.snapping() {
            snapshots += 1;
        }
        most_erased = most_erased.max(done.erased);
        most_entries = most_entries.max(done.entries);
    }
    assert!(
        snapshots >= 3,
        "the log came round to {snapshots} snapshots"
    );
    assert!(
        most_erased <= 3 && most_entries <= 40,
        "a bounded write wrote {most_entries} records and erased {most_erased} sectors"
    );
    let (_, replayed) = RowLog::open(&mut flash, TAG).expect("reopen");
    let kept = (0..peers)
        .filter(|n| replayed.index.kept.contains_key(&peer(*n)))
        .count();
    assert_eq!(kept, peers as usize, "every row survives the ring");
}

/// The index files received entries by their source and sent ones by
/// their destination, a mark into the marks, and counts the spans' every
/// sector once.
#[test]
fn the_index_files_entries_by_conversation() {
    let mut packed = vec![0u8; 100];
    packed[..16].copy_from_slice(&[0xD0; 16]);
    packed[16..32].copy_from_slice(&[0x50; 16]);
    let received = Entry {
        seq: 1,
        kind: KIND_RECEIVED,
        at: 0,
        packed: packed.clone(),
    };
    let sent = Entry {
        seq: 2,
        kind: KIND_SENT,
        at: 0,
        packed,
    };
    assert_eq!(wire_peer(&received), Some([0x50; 16]));
    assert_eq!(wire_peer(&sent), Some([0xD0; 16]));
    let mut index = Index::default();
    index.take(&received, (3, 4), true);
    index.take(&received, (4, 4), true);
    index.take(&sent, (6, 6), false);
    index.take(
        &Entry {
            seq: 0,
            kind: KIND_MARK,
            at: 0,
            packed: Vec::new(),
        },
        (5, 5),
        false,
    );
    assert_eq!(index.kept[&[0x50; 16]].sectors, [3, 4]);
    assert_eq!(index.kept[&[0x50; 16]].entries, 2);
    assert_eq!(index.loaded[&[0x50; 16]], (1, 2));
    assert_eq!(index.kept[&[0xD0; 16]].sectors, [6]);
    assert!(!index.loaded.contains_key(&[0xD0; 16]));
    assert_eq!(index.marks, [5]);
}

/// 1556: a mark naming a message the index learnt files its sector
/// under that message's conversation; one naming a message it never saw
/// stays with the unplaced marks, and one naming both goes to both. The
/// conversation owes its record again, which carries the sector.
#[test]
fn a_mark_files_under_the_conversation_it_names() {
    use crate::home::wire::{mark_request, Mark, MarkFlag};
    let mut packed = vec![0u8; 100];
    packed[16..32].copy_from_slice(&[0x50; 16]);
    let received = Entry {
        seq: 1,
        kind: KIND_RECEIVED,
        at: 0,
        packed,
    };
    let mark = |ids: Vec<[u8; 32]>| Entry {
        seq: 0,
        kind: KIND_MARK,
        at: 0,
        packed: mark_request(&Mark {
            flag: MarkFlag::Read,
            ids,
        }),
    };
    let mut index = Index::default();
    index.learn(&received, (3, 3), &[0xA1; 32]);
    index.take(&received, (3, 3), true);
    let _ = index.owed_records();
    index.take(&mark(vec![[0xA1; 32]]), (7, 7), false);
    assert_eq!(index.kept[&[0x50; 16]].sectors, [3, 7]);
    assert!(index.marks.is_empty(), "a placed mark is no ring-wide one");
    assert!(index.owes(), "the record carries the mark's sector");
    index.take(&mark(vec![[0xB2; 32]]), (8, 8), false);
    assert_eq!(index.kept[&[0x50; 16]].sectors, [3, 7]);
    assert_eq!(index.marks, [8]);
    index.take(&mark(vec![[0xA1; 32], [0xB2; 32]]), (9, 9), false);
    assert_eq!(index.kept[&[0x50; 16]].sectors, [3, 7, 9]);
    assert_eq!(index.marks, [8, 9]);
}

/// 1556: a state an older image wrote, without the byte after its asks,
/// reads as one whose marks are not filed by conversation.
#[test]
fn a_state_without_its_last_byte_is_unplaced() {
    let written = state(5);
    let mut bytes = written.encode();
    assert_eq!(State::decode(&bytes), Some(written.clone()));
    bytes.pop();
    let old = State::decode(&bytes).expect("an older state reads");
    assert!(!old.placed);
    assert_eq!(old.marks, written.marks);
}

/// The partition splits an eighth off for the log; a small one stays
/// whole.
#[test]
fn the_partition_splits_an_eighth() {
    assert_eq!(split(1024), Some((896, 128)));
    assert_eq!(split(32), None);
    assert_ne!(rows_tag(TAG), TAG);
}
