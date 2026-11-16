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
    let (store, restored, _asks) = Store::open(flash, tag, |e| got.push(e)).expect("open");
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

/// The idle-time erase (1493): with the ring's next sector pre-erased,
/// a page that rolls into it erases nothing on the page path, the counts
/// agree with a boot's, and the oldest sector's entries are paid at the
/// pre-erase, not later.
#[test]
fn a_pre_erased_sector_costs_the_page_path_no_erase() {
    // One entry fills one sector to 8 spare bytes: 4023 payload + 17
    // entry head + 8 frame + the 16-byte cursor mark = 4064 of the 4072
    // a sector has after its header, and 8 is under a frame's worth, so
    // every append opens exactly one sector.
    let full = 4023;
    let mut flash = MemFlash::new(4);
    let (mut store, ..) = reopen(&mut flash, TAG);
    for seq in 1..=4u64 {
        store
            .append(&mut flash, &[entry(seq, full)], seq)
            .expect("append");
    }
    assert_eq!((store.used(), flash.erases), (4, 4));
    // The quiet moment: one call erases ahead, the second finds it done.
    assert!(store.pre_erase(&mut flash).expect("pre-erase"));
    assert!(!store.pre_erase(&mut flash).expect("again"));
    assert_eq!(
        (store.entries(), flash.erases),
        (3, 5),
        "the erased sector's entries are paid at the pre-erase"
    );
    // The page that rolls into the pre-erased sector: writes, no erase.
    let done = store
        .append(&mut flash, &[entry(5, full)], 5)
        .expect("page");
    assert_eq!((done.erased, flash.erases), (0, 5));
    let (reopened, restored, _) = reopen(&mut flash, TAG);
    assert_eq!(restored.torn, 0);
    assert_eq!(reopened.cursor(), 5);
    assert_eq!(reopened.entries(), store.entries());
    assert_eq!(reopened.used(), store.used());
}

/// A fresh store: pre-erase finds erased flash, costs no erase, and never
/// touches the open tail sector.
#[test]
fn pre_erase_leaves_erased_flash_and_the_tail_alone() {
    let mut flash = MemFlash::new(4);
    let (mut store, ..) = reopen(&mut flash, TAG);
    assert!(!store.pre_erase(&mut flash).expect("erased already"));
    assert_eq!(flash.erases, 0);
    store.append(&mut flash, &[entry(1, 50)], 1).expect("page");
    // The next sector is erased flash; the tail holds frames and stays.
    assert!(!store.pre_erase(&mut flash).expect("next is erased"));
    let (_, restored, got) = reopen(&mut flash, TAG);
    assert_eq!((restored.torn, got.len()), (0, 1));
}

/// A closed tail (1540, the M9 bench's owe): the next write opens the
/// next sector with an erase, even one a quiet moment found erased, and
/// what both sectors hold reads back whole.
#[test]
fn a_closed_tail_makes_the_next_write_erase_and_loses_nothing() {
    let mut flash = MemFlash::new(4);
    let (mut store, ..) = reopen(&mut flash, TAG);
    store.append(&mut flash, &[entry(1, 50)], 1).expect("page");
    assert!(!store.pre_erase(&mut flash).expect("next is erased"));
    let erases = flash.erases;
    store.close();
    let done = store.append(&mut flash, &[entry(2, 50)], 2).expect("page");
    assert_eq!((done.erased, done.writes), (1, 1));
    assert_eq!(flash.erases, erases + 1);
    // Without the close the same write goes into the open sector.
    let done = store.append(&mut flash, &[entry(3, 50)], 3).expect("page");
    assert_eq!(done.erased, 0);
    let (reopened, restored, got) = reopen(&mut flash, TAG);
    assert_eq!((restored.torn, got.len()), (0, 3));
    assert_eq!(reopened.cursor(), 3);
    assert_eq!(reopened.used(), 2);
}

/// Asks kept with the pages (1493): queued asks come back at boot oldest
/// first, a done one does not, and entries around them still read.
#[test]
fn asks_come_back_at_boot_unless_done() {
    let mut flash = MemFlash::new(8);
    let (mut store, ..) = reopen(&mut flash, TAG);
    store.append(&mut flash, &[entry(1, 100)], 1).expect("page");
    store
        .keep_ask(&mut flash, &[0xA1; 16], 2, b"mark-bytes")
        .expect("ask one");
    store
        .keep_ask(&mut flash, &[0xB2; 16], 1, &vec![7u8; 5000])
        .expect("ask two, split across sectors");
    store.append(&mut flash, &[entry(2, 100)], 2).expect("page");
    store.ask_done(&mut flash, &[0xA1; 16]).expect("done one");
    let mut got = Vec::new();
    let (store, restored, asks) = Store::open(&mut flash, TAG, |e| got.push(e)).expect("open");
    assert_eq!(restored.torn, 0);
    assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(store.entries(), 2, "asks and dones are no entries");
    assert_eq!(asks.len(), 1, "the done ask stays done");
    assert_eq!((asks[0].id, asks[0].tag), ([0xB2; 16], 1));
    assert_eq!(asks[0].request, vec![7u8; 5000]);
}

/// A power loss inside an ask's write: the boot queues it again or not at
/// all, and never hands back half a request.
#[test]
fn a_cut_ask_is_whole_or_gone() {
    let mut flash = MemFlash::new(8);
    let (mut store, ..) = reopen(&mut flash, TAG);
    store.append(&mut flash, &[entry(1, 100)], 1).expect("page");
    flash.cut_at = Some(flash.writes + 1);
    let cut = store.keep_ask(&mut flash, &[0xC3; 16], 1, &vec![9u8; 600]);
    assert_eq!(cut, Err(MemError::Cut));
    flash.cut_at = None;
    let mut got = Vec::new();
    let (_, restored, asks) = Store::open(&mut flash, TAG, |e| got.push(e.seq)).expect("open");
    assert_eq!(restored.torn, 1);
    assert_eq!(got, [1], "the entry before the cut still reads");
    assert!(asks.is_empty(), "half an ask is no ask");
}

/// 1563: a store read after an end it ended at once hands back what came
/// after it alone, across sectors and with an entry that runs over two;
/// an end whose sector the ring took back, or one past the log's end, is
/// no place to read after.
#[test]
fn open_after_reads_the_tail_alone() {
    let mut flash = MemFlash::new(8);
    let (mut store, ..) = reopen(&mut flash, TAG);
    let before: Vec<_> = (1..=10).map(|s| entry(s, 300)).collect();
    store.append(&mut flash, &before, 10).expect("before");
    let end = store.end();
    let after: Vec<_> = (11..=30).map(|s| entry(s, 300)).collect();
    store.append(&mut flash, &after, 31).expect("after");
    let mut got = Vec::new();
    let (tail, restored, asks, _, done) =
        Store::open_after(&mut flash, TAG, end, &|| 0, |e, _| got.push(e))
            .expect("read")
            .expect("a place in the log");
    assert_eq!(got, after);
    assert_eq!(restored.entries, 20);
    assert!(asks.is_empty() && done.is_empty());
    assert_eq!(tail.cursor(), 31, "the tail's mark");
    assert_eq!(tail.end(), store.end());
    // Read at the log's end: nothing.
    let mut none = Vec::new();
    let (_, restored, ..) =
        Store::open_after(&mut flash, TAG, store.end(), &|| 0, |e, _| none.push(e))
            .expect("read")
            .expect("the end is a place");
    assert!(none.is_empty() && restored.entries == 0);
    // Past the end, and a sector the ring never held.
    let (seq, at) = store.end();
    assert!(
        Store::open_after(&mut flash, TAG, (seq, at + 4), &|| 0, |_, _| {})
            .expect("read")
            .is_none()
    );
    assert!(
        Store::open_after(&mut flash, TAG, (seq + 5, 24), &|| 0, |_, _| {})
            .expect("read")
            .is_none()
    );
}

/// `forget` (1562): the entries it names are gone after a reboot, a long
/// one that ran over three sectors with them, every other entry reads
/// back whole and in order, the cursor, an ask and nothing torn stay, the
/// store appends after a forget as before, and a forget that names
/// nothing writes nothing.
#[test]
fn forget_takes_the_named_entries_out_and_leaves_the_rest_whole() {
    let mut flash = MemFlash::new(16);
    let (mut store, ..) = reopen(&mut flash, TAG);
    let mut all: Vec<Entry> = (1..=30)
        .map(|s| entry(s, 150 + (s as usize % 7) * 40))
        .collect();
    // The long one runs on over sectors; the 1-length is the drop's sign.
    all.insert(10, entry(100, 9_000));
    for page in all.chunks(4) {
        let last = page.iter().map(|e| e.seq).max().unwrap_or(0);
        store.append(&mut flash, page, last).expect("append");
    }
    store
        .keep_ask(&mut flash, &[7; 16], 1, b"a request")
        .expect("ask");
    store.append(&mut flash, &[], 200).expect("mark");
    let named = |e: &Entry| e.seq.is_multiple_of(3) || e.seq == 100;
    let (writes, erases) = (flash.writes, flash.erases);
    let forgot = Store::forget(&mut flash, TAG, |_| false, &|| ()).expect("nothing");
    assert_eq!(forgot, Forgot::default());
    assert_eq!(
        (flash.writes, flash.erases),
        (writes, erases),
        "nothing named, nothing written"
    );
    let fed = core::cell::Cell::new(0u32);
    let forgot = Store::forget(&mut flash, TAG, named, &|| fed.set(fed.get() + 1)).expect("forget");
    assert_eq!(forgot.entries, 11, "ten of thirty and the long one");
    assert!(forgot.frames >= 13, "the long one's parts: {forgot:?}");
    assert!(fed.get() >= forgot.sectors, "fed per sector");
    let mut asks = Vec::new();
    let mut got = Vec::new();
    let (store, restored, pending) = Store::open(&mut flash, TAG, |e| got.push(e)).expect("open");
    asks.extend(pending);
    let want: Vec<Entry> = all.iter().filter(|e| !named(e)).cloned().collect();
    assert_eq!(got, want);
    assert_eq!(restored.torn, 0);
    assert_eq!(store.cursor(), 200, "the mark stays");
    assert_eq!(asks.len(), 1, "the ask stays");
    // The ring goes on after a forget.
    let mut store = store;
    store
        .append(&mut flash, &[entry(201, 120)], 201)
        .expect("after");
    let (_, _, got) = reopen(&mut flash, TAG);
    assert_eq!(got.last().map(|e| e.seq), Some(201));
    assert_eq!(got.len(), want.len() + 1);
}
