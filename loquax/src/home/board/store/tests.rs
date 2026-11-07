//! The message store on a flash in memory that keeps the part's rules
//! (1476): what is written reads back after a "reboot", a page is one
//! write, the ring erases its oldest sector, a cut write costs only the
//! frame it cut, and another home's sectors are not shown.

extern crate std;

use std::vec::Vec;

use super::*;
use crate::home::KIND_RECEIVED;

const TAG: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

fn entry(seq: u64, len: usize) -> Entry {
    Entry {
        seq,
        kind: KIND_RECEIVED,
        at: 1_762_000_000 + seq as i64,
        packed: (0..len).map(|i| (seq as usize + i) as u8).collect(),
    }
}

fn reopen(flash: &mut MemFlash, tag: [u8; 8]) -> (Store, Restored, Vec<Entry>) {
    let mut got = Vec::new();
    let (store, restored) = Store::open(flash, tag, |e| got.push(e)).expect("open");
    (store, restored, got)
}

/// Pages written, the board "rebooted": every entry back, in order, the
/// cursor at the newest page's end (a mark past the last kept entry), one
/// flash write per page that fit a sector, and the second boot appends
/// after the first's frames without erasing anything.
#[test]
fn pages_read_back_after_a_reboot_one_write_a_page() {
    let mut flash = MemFlash::new(16);
    let (mut store, restored, got) = reopen(&mut flash, TAG);
    assert_eq!(
        (restored, got.len(), store.cursor()),
        (Restored::default(), 0, 0)
    );
    let first: Vec<_> = (1..=5).map(|s| entry(s, 120)).collect();
    let done = store.append(&mut flash, &first, 7).expect("append");
    assert_eq!(done.entries, 5);
    assert_eq!(done.erased, 1, "the first sector opened");
    assert_eq!(done.writes, 1, "one write for the page's frames");
    let (mut store, restored, got) = reopen(&mut flash, TAG);
    assert_eq!(got, first);
    assert_eq!(restored.entries, 5);
    assert_eq!(store.cursor(), 7, "the mark of the page's last sequence");
    let erases = flash.erases;
    let more: Vec<_> = (8..=9).map(|s| entry(s, 300)).collect();
    let done = store.append(&mut flash, &more, 9).expect("append");
    assert_eq!(
        (done.writes, done.erased),
        (1, 0),
        "appended to the open sector"
    );
    assert_eq!(flash.erases, erases);
    let (store, _, got) = reopen(&mut flash, TAG);
    assert_eq!(got.len(), 7);
    assert_eq!(got[5..], more[..]);
    assert_eq!((store.cursor(), store.entries(), store.used()), (9, 7, 1));
}

/// A page the fold kept nothing of still moves the store's end; an entry
/// longer than a sector runs on over three and reads back whole.
#[test]
fn a_mark_moves_the_end_and_a_long_entry_spans_sectors() {
    let mut flash = MemFlash::new(8);
    let (mut store, ..) = reopen(&mut flash, TAG);
    store.append(&mut flash, &[], 40).expect("mark");
    let long = entry(41, 10_000);
    let done = store
        .append(&mut flash, std::slice::from_ref(&long), 41)
        .expect("long");
    assert_eq!(done.entries, 1);
    assert!(done.writes >= 3, "{done:?}");
    let (store, restored, got) = reopen(&mut flash, TAG);
    assert_eq!(got, std::vec![long]);
    assert_eq!(restored.torn, 0);
    assert_eq!(store.cursor(), 41);
}

/// The ring: once every sector is used the oldest is erased for the next,
/// the store keeps the newest entries and nothing torn, and the erases
/// are one per sector filled.
#[test]
fn a_full_ring_erases_its_oldest_sector() {
    let mut flash = MemFlash::new(4);
    let (mut store, ..) = reopen(&mut flash, TAG);
    for seq in 1..=200u64 {
        store
            .append(&mut flash, &[entry(seq, 200)], seq)
            .expect("append");
    }
    let (store, restored, got) = reopen(&mut flash, TAG);
    assert_eq!(restored.torn, 0);
    assert_eq!(store.cursor(), 200);
    assert_eq!(got.last().map(|e| e.seq), Some(200));
    let seqs: Vec<u64> = got.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    // 200 entries of 225 bytes framed fill ~11 sectors: erased 11 times
    // over four, the newest three and a part kept.
    assert!(got.len() < 200 && got.len() > 40, "{}", got.len());
    assert_eq!(store.used(), 4);
    assert!((10..=13).contains(&flash.erases), "{}", flash.erases);
}

/// A power loss in the middle of a page's write: the store reads back
/// everything before the cut frame, the cursor is the last whole entry,
/// and the next page goes into a fresh sector, never over the torn bytes.
#[test]
fn a_cut_write_costs_only_its_frame() {
    let mut flash = MemFlash::new(8);
    let (mut store, ..) = reopen(&mut flash, TAG);
    store
        .append(&mut flash, &[entry(1, 100), entry(2, 100)], 2)
        .expect("first page");
    flash.cut_at = Some(flash.writes + 1);
    let cut = store.append(&mut flash, &[entry(3, 400), entry(4, 400)], 4);
    assert_eq!(cut, Err(MemError::Cut));
    flash.cut_at = None;
    let (mut store, restored, got) = reopen(&mut flash, TAG);
    assert_eq!(restored.torn, 1);
    let seqs: Vec<u64> = got.iter().map(|e| e.seq).collect();
    assert!(seqs.starts_with(&[1, 2]), "{seqs:?}");
    assert!(store.cursor() < 4, "the cut page's mark never landed");
    let done = store
        .append(&mut flash, &[entry(3, 400), entry(4, 400)], 4)
        .expect("again");
    assert_eq!(done.erased, 1, "a fresh sector, not the torn one");
    let (store, _, got) = reopen(&mut flash, TAG);
    assert_eq!(store.cursor(), 4);
    assert_eq!(got.last().map(|e| e.seq), Some(4));
}

/// Another home's sectors are not shown and its store is empty for this
/// one; this home's pages go after them in the ring.
#[test]
fn another_homes_sectors_are_not_shown() {
    let mut flash = MemFlash::new(8);
    let (mut theirs, ..) = reopen(&mut flash, [9; 8]);
    theirs
        .append(&mut flash, &[entry(1, 50)], 1)
        .expect("theirs");
    let (mut ours, restored, got) = reopen(&mut flash, TAG);
    assert!(got.is_empty());
    assert_eq!((restored.foreign, ours.cursor()), (1, 0));
    ours.append(&mut flash, &[entry(5, 50)], 5).expect("ours");
    let (ours, _, got) = reopen(&mut flash, TAG);
    assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), std::vec![5]);
    assert_eq!(ours.cursor(), 5);
    let (_, _, theirs) = reopen(&mut flash, [9; 8]);
    assert_eq!(theirs.len(), 1);
}

/// An erased flash, one of zeros and one of noise open as empty stores
/// and never panic.
#[test]
fn erased_zero_and_noise_open_empty() {
    for fill in [0xFFu8, 0x00, 0x5A] {
        let mut flash = MemFlash::new(4);
        for s in &mut flash.sectors {
            s.fill(fill);
        }
        let (store, restored, got) = reopen(&mut flash, TAG);
        assert!(got.is_empty(), "{fill:#x}");
        assert_eq!((store.cursor(), restored.sectors), (0, 0));
    }
}

/// The CRC is the zlib one: the check value of "123456789".
#[test]
fn the_crc_is_crc32() {
    assert_eq!(crc32_parts(b"1234", b"56789"), 0xCBF4_3926);
}
