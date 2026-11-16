//! A TLSF heap whose control block lives inside the region it manages
//! (order 1574).
//!
//! `embedded_alloc::TlsfHeap` holds rlsf's control block (the first- and
//! second-level bitmaps and the 32 x 32 table of free-list heads, 4228 B
//! with `usize` bitmaps on a 32-bit target) inside the allocator value,
//! which is a `static`, which is `.bss`. On the ESP32-S3 core 0's stack is
//! whatever internal DRAM `.bss` leaves over, so 1569's switch from llff
//! to tlsf took exactly those 4228 B off the stack and the M9 image fell
//! below its 111000 B floor.
//!
//! [`PoolTlsf::place`] writes the control block at the (aligned) start of
//! the region with rlsf's const `Tlsf::new()` and hands the rest of the
//! region to it as the pool (`Tlsf::insert_free_block_ptr`). The static
//! keeps only two pointers. The allocator's algorithm is unchanged: the
//! same rlsf type with the same bitmap widths and list lengths as
//! `embedded_alloc::TlsfHeap`.
//!
//! The module needs nothing but `core` and `rlsf`, and it has no lock:
//! the firmware wraps it in its critical section (`lib.rs`), and the host
//! lab (`leviculum-heap-lab`) includes this same file to measure and test
//! it.

use core::alloc::Layout;
use core::mem::{align_of, size_of};
use core::ptr::NonNull;

/// The rlsf instantiation `embedded_alloc::TlsfHeap` uses: `usize`
/// bitmaps, as many first- and second-level lists as a `usize` has bits.
pub type Tlsf =
    rlsf::Tlsf<'static, usize, usize, { usize::BITS as usize }, { usize::BITS as usize }>;

/// A TLSF heap placed in its own region: the control block at the front,
/// the pool behind it.
pub struct PoolTlsf {
    tlsf: NonNull<Tlsf>,
    pool: NonNull<[u8]>,
}

// SAFETY: a `PoolTlsf` owns its region exclusively (the contract of
// `place`); moving the handle to another thread moves nothing the old
// one could still reach. Every use goes through `&mut self` or the
// caller's lock.
unsafe impl Send for PoolTlsf {}

impl PoolTlsf {
    /// The bytes the control block takes from the region, alignment
    /// slack not counted.
    pub const CONTROL_BYTES: usize = size_of::<Tlsf>();

    /// Place a heap in the `size` bytes at `start`: the control block at
    /// the first address aligned for it, the pool from its end to the end
    /// of the region. `None` when the region cannot hold the control
    /// block and a pool rlsf accepts (the region is then untouched past
    /// the control block and owed to nobody).
    ///
    /// # Safety
    ///
    /// The region is mapped, writable, and used by nothing else for as
    /// long as the returned heap or any block it hands out lives.
    pub unsafe fn place(start: usize, size: usize) -> Option<Self> {
        let control = start.checked_next_multiple_of(align_of::<Tlsf>())?;
        let pool_start = control.checked_add(Self::CONTROL_BYTES)?;
        let end = start.checked_add(size)?;
        let pool_len = end.checked_sub(pool_start)?;
        if pool_len == 0 {
            return None;
        }
        let tlsf = NonNull::new(control as *mut Tlsf)?;
        let pool_ptr = NonNull::new(pool_start as *mut u8)?;
        // SAFETY: `control` is aligned for `Tlsf` and its CONTROL_BYTES
        // lie inside the caller's region, which nothing else uses.
        unsafe { tlsf.as_ptr().write(Tlsf::new()) };
        // SAFETY: the pool is the rest of the caller's region, disjoint
        // from the control block, and lives as long as the heap.
        let taken = unsafe {
            (*tlsf.as_ptr())
                .insert_free_block_ptr(NonNull::slice_from_raw_parts(pool_ptr, pool_len))
        }?;
        // rlsf's `iter_blocks` wants the pool's start with the length
        // `insert_free_block_ptr` reported, not the length offered.
        let pool = NonNull::slice_from_raw_parts(pool_ptr, taken.get());
        Some(Self { tlsf, pool })
    }

    /// A block for `layout`, or `None` when no free block fits.
    pub fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        // SAFETY: `place` wrote a live `Tlsf` there, and `&mut self` is
        // the only way to it.
        unsafe { self.tlsf.as_mut() }.allocate(layout)
    }

    /// Give back a block.
    ///
    /// # Safety
    ///
    /// `ptr` came from [`Self::allocate`] on this heap with alignment
    /// `align` and has not been given back since.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, align: usize) {
        // SAFETY: as in `allocate`; the block's provenance is the
        // caller's contract.
        unsafe { self.tlsf.as_mut().deallocate(ptr, align) }
    }

    /// The payload bytes of all free blocks: a walk of every block in the
    /// pool, the way `embedded_alloc::TlsfHeap::free` counts them.
    pub fn free(&self) -> usize {
        // SAFETY: `place` wrote a live `Tlsf`, and `self.pool` is the one
        // pool it was given.
        unsafe {
            self.tlsf
                .as_ref()
                .iter_blocks(self.pool)
                .filter(|block| !block.is_occupied())
                .map(|block| block.max_payload_size())
                .sum()
        }
    }

    /// The pool the heap hands blocks out of: the region less the control
    /// block in front of it.
    pub fn pool(&self) -> NonNull<[u8]> {
        self.pool
    }

    /// Where the control block sits.
    pub fn control(&self) -> NonNull<Tlsf> {
        self.tlsf
    }
}
