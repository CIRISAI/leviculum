//! Height pieces against hostile lengths (order 1740): a length field that
//! claims more than the piece holds is refused before anything sized by it
//! is allocated, and a zstd frame that claims a huge window or decodes to
//! more than one tile is refused with the decoder's memory bounded. Counted
//! by a global allocator that tallies the calling thread only, so the other
//! tests of this binary running in parallel do not disturb the count.

// A global allocator is `unsafe impl` by its trait; it only forwards to
// `System` and counts. The crate denies unsafe code, a test may allow it.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use leviculum_map::elevation::{
    split, Block, Directory, ElevationError, TileDecoder, TileFault, TILE_BYTES, TILE_CELLS,
};

const CHEBBI: &[u8] = include_bytes!("../fixtures/erg-chebbi.lmh");

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static TOTAL: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ON.try_with(|on| {
            if on.get() {
                let live = LIVE.with(|l| {
                    l.set(l.get() + layout.size());
                    l.get()
                });
                PEAK.with(|p| p.set(p.get().max(live)));
                TOTAL.with(|t| t.set(t.get() + layout.size()));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = ON.try_with(|on| {
            if on.get() {
                LIVE.with(|l| l.set(l.get().saturating_sub(layout.size())));
            }
        });
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Run `f` with this thread's allocations counted; returns its result, the
/// peak of live bytes allocated inside it and the bytes allocated in all.
fn counted<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    TOTAL.with(|t| t.set(0));
    ON.with(|on| on.set(true));
    let out = f();
    ON.with(|on| on.set(false));
    (out, PEAK.with(Cell::get), TOTAL.with(Cell::get))
}

fn ranges() -> Vec<std::ops::Range<usize>> {
    split(CHEBBI).expect("split the fixture").1
}

#[test]
fn a_hostile_tile_length_is_refused_without_allocating() {
    let r = ranges();
    let mut bad = CHEBBI[r[1].clone()].to_vec();
    // The first tile's compressed length: four gigabytes.
    bad[24..28].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    let (got, peak, total) = counted(|| Block::parse(&bad).err());
    assert_eq!(got, Some(ElevationError::Length));
    assert_eq!((peak, total), (0, 0), "nothing allocated");
}

#[test]
fn a_hostile_block_count_and_text_length_are_refused_without_allocating() {
    let r = ranges();
    let mut bad = CHEBBI[r[0].clone()].to_vec();
    bad[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    let (got, peak, _) = counted(|| Directory::parse(&bad).err());
    assert_eq!(got, Some(ElevationError::Length));
    assert_eq!(peak, 0);

    let mut bad = CHEBBI.to_vec();
    bad[32..36].copy_from_slice(&u32::MAX.to_le_bytes()); // attribution length
    let (got, peak, total) = counted(|| split(&bad).err());
    assert_eq!(got, Some(ElevationError::Length));
    assert_eq!((peak, total), (0, 0), "split refuses before its range list");
}

#[test]
fn a_frame_claiming_a_huge_window_is_refused_without_allocating_it() {
    let mut decoder = TileDecoder::new();
    let mut out = vec![0i16; TILE_CELLS];
    // Magic, a descriptor with no content size and no single segment, and a
    // window descriptor of 2^30 bytes, then one empty last raw block.
    let frame = [0x28, 0xB5, 0x2F, 0xFD, 0x00, 20 << 3, 0x01, 0x00, 0x00];
    let (got, peak, _) = counted(|| decoder.decode(&frame, &mut out));
    assert_eq!(got, Err(TileFault::Frame));
    assert!(peak < 64 * 1024, "peak {peak} bytes");
}

#[test]
fn a_frame_decoding_past_one_tile_is_refused_with_bounded_memory() {
    let mut decoder = TileDecoder::new();
    let mut out = vec![0i16; TILE_CELLS];
    // A window of one tile (2^17), checksum flag set, then 1,000 RLE blocks
    // of 128 KiB each from one byte apiece: 128 MiB from 4 KiB of input.
    let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0x04, 7 << 3];
    let size = 128 * 1024u32;
    for n in 0..1000 {
        let last = u32::from(n == 999);
        let header = last | (1 << 1) | (size << 3);
        frame.extend_from_slice(&header.to_le_bytes()[..3]);
        frame.push(0x2a);
    }
    frame.extend_from_slice(&[0, 0, 0, 0]);
    let (got, peak, _) = counted(|| decoder.decode(&frame, &mut out));
    assert_eq!(got, Err(TileFault::Size));
    assert!(peak < 1024 * 1024, "peak {peak} bytes");
}

#[test]
fn a_tile_decodes_in_a_bounded_working_set() {
    // What a board must hold to read heights: the decoder with its planes
    // and window, and one tile of output (the derived zooms add one tile of
    // scratch). Pinned so a dependency bump that grows it is seen.
    let r = ranges();
    let block_bytes = CHEBBI[r[1].clone()].to_vec();
    let (sum, peak, _) = counted(|| {
        let block = Block::parse(&block_bytes).expect("parse");
        let mut decoder = TileDecoder::new();
        let mut out = vec![0i16; TILE_CELLS];
        let mut scratch = vec![0i16; TILE_CELLS];
        block
            .heights(&mut decoder, 12, 2002, 1674, &mut out, &mut scratch)
            .expect("decode");
        out.iter().map(|&h| i64::from(h)).sum::<i64>()
    });
    assert!(sum > 0);
    let floor = TILE_BYTES * 3; // planes, out, scratch
    eprintln!("ELEVATION working_set_bytes={peak}");
    assert!(
        peak >= floor && peak < floor + 300 * 1024,
        "peak {peak} bytes against {floor} for the three tile buffers"
    );
}
