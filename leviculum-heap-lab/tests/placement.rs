//! 1574: the firmware's TLSF keeps its control block inside the region it
//! manages, not in `.bss`.
//!
//! `embedded_alloc::TlsfHeap` held rlsf's 4228 B control block in its
//! `static`; on the ESP32-S3 `.bss` comes off core 0's stack, and the M9
//! image fell 3900 B below its stack floor. `leviculum-esp/src/
//! pool_tlsf.rs` (the file the lab includes, [`pool_tlsf`]) writes the
//! control block at the front of the region and gives rlsf the rest. These
//! tests hold what that placement owes: the control block inside the
//! region and aligned, no block ever handed out over it, every byte given
//! back, and a region too small refused rather than overrun.

use core::alloc::Layout;
use core::mem::{align_of, size_of};
use core::ptr::NonNull;

use leviculum_heap_lab::pool_tlsf::{PoolTlsf, Tlsf};

/// A region from the system allocator (the lab's router serves the
/// system while no `with` runs), 8-aligned, leaked for the test's life.
fn region(bytes: usize) -> usize {
    let words = vec![0u64; bytes.div_ceil(8)].into_boxed_slice();
    Box::leak(words).as_mut_ptr() as usize
}

#[test]
fn control_block_is_the_bitmaps_and_the_list_heads() {
    // fl_bitmap, sl_bitmap[FLLEN], first_free[FLLEN][SLLEN]: on the
    // 32-bit ESP32-S3 4 * (1 + 32 + 32 * 32) = 4228 B, the bytes 1569's
    // `.bss` grew by.
    let bits = usize::BITS as usize;
    assert_eq!(
        PoolTlsf::CONTROL_BYTES,
        size_of::<usize>() * (1 + bits + bits * bits)
    );
    assert_eq!(PoolTlsf::CONTROL_BYTES, size_of::<Tlsf>());
}

#[test]
fn control_block_sits_aligned_at_the_front_and_the_pool_behind_it() {
    const SIZE: usize = 256 * 1024;
    let base = region(SIZE + 8);
    // An aligned start and one the placement has to round up.
    for start in [base, base + 1] {
        let size = SIZE - (start - base);
        // SAFETY: the leaked region is ours alone and outlives the heap.
        let Some(heap) = (unsafe { PoolTlsf::place(start, size) }) else {
            panic!("a {size} B region was refused");
        };
        let control = heap.control().as_ptr() as usize;
        let pool = heap.pool();
        let pool_start = pool.as_ptr() as *mut u8 as usize;
        let pool_end = pool_start + pool.len();
        assert_eq!(control % align_of::<Tlsf>(), 0);
        assert!(control >= start && control - start < align_of::<Tlsf>());
        assert!(pool_start >= control + PoolTlsf::CONTROL_BYTES);
        assert!(pool_end <= start + size);
        // What the region loses to the placement: the control block and
        // at most the alignment slack in front of it and rlsf's own
        // rounding at the pool's ends.
        assert!(size - pool.len() <= PoolTlsf::CONTROL_BYTES + 4 * align_of::<Tlsf>());
    }
}

#[test]
fn no_block_is_ever_handed_out_over_the_control_block() {
    const SIZE: usize = 128 * 1024;
    let start = region(SIZE);
    // SAFETY: the leaked region is ours alone and outlives the heap.
    let Some(mut heap) = (unsafe { PoolTlsf::place(start, SIZE) }) else {
        panic!("a {SIZE} B region was refused");
    };
    let control = heap.control().as_ptr() as usize;
    let control_end = control + PoolTlsf::CONTROL_BYTES;
    let free_at_start = heap.free();
    assert!(free_at_start > SIZE - PoolTlsf::CONTROL_BYTES - 256);

    // Fill the heap with mixed sizes until it refuses, then give
    // everything back: every block lies outside the control block and
    // inside the region, and the free bytes come back whole.
    let sizes = [16usize, 48, 200, 1024, 24, 4096, 8, 333];
    let mut held: Vec<(NonNull<u8>, Layout)> = Vec::new();
    let mut i = 0;
    loop {
        let Ok(layout) = Layout::from_size_align(sizes[i % sizes.len()], 8) else {
            panic!("a valid layout");
        };
        let Some(ptr) = heap.allocate(layout) else {
            break;
        };
        let at = ptr.as_ptr() as usize;
        assert!(at >= control_end || at + layout.size() <= control);
        assert!(at >= start && at + layout.size() <= start + SIZE);
        held.push((ptr, layout));
        i += 1;
    }
    assert!(held.len() > 100, "only {} blocks fitted", held.len());
    assert!(heap.free() < free_at_start / 8);
    for (ptr, layout) in held.drain(..).rev() {
        // SAFETY: each block came from this heap with this alignment.
        unsafe { heap.deallocate(ptr, layout.align()) };
    }
    assert_eq!(heap.free(), free_at_start);
}

#[test]
fn a_region_without_room_for_the_control_block_and_a_pool_is_refused() {
    let start = region(PoolTlsf::CONTROL_BYTES + 64);
    for size in [0, 1, PoolTlsf::CONTROL_BYTES - 1, PoolTlsf::CONTROL_BYTES] {
        // SAFETY: the leaked region is ours alone; a refusal writes at
        // most the control block, inside it.
        assert!(
            unsafe { PoolTlsf::place(start, size) }.is_none(),
            "a {size} B region was accepted"
        );
    }
    // An end past the address space is refused, not wrapped.
    // SAFETY: refused before anything is written.
    assert!(unsafe { PoolTlsf::place(usize::MAX - 8, 4096) }.is_none());
}
