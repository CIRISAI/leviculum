//! One TLSF heap per core, each behind its own lock (order 1594).
//!
//! Until 1594 the M9 had one heap behind the one cross-core critical
//! section, so every allocation of the UI core waited for whatever core 0
//! held that section for: its own allocations, its queue pushes, the
//! UART interrupt, and the flash ops (`flash::parked`). Window 19
//! measured the UI core inside the allocator for about 1 s of every 5
//! during the pull (`[IDLE] alloc_ms1` median 1045), while it built
//! eleven rows in 338 ms that the host builds in 28 us.
//!
//! [`Pools`] gives every core a pool of its own: an allocation goes to
//! the pool of the core that asks ([`Pools::allocate`]'s `home`), a free
//! goes to the pool whose region holds the pointer, whichever core frees
//! it ([`Pools::deallocate`]). Each pool has its own lock, so a core
//! meets the other only when it frees a block the other made, and then
//! only for the one TLSF step the other may be in. A pool that is full,
//! or was never placed, hands the request on to the next one, so a pool
//! sized too small costs contention, never an allocation failure.
//!
//! The module needs nothing but `core` and [`crate::pool_tlsf`], and the
//! lock is the caller's ([`PoolLock`]): the firmware's turns interrupts
//! off on its core and spins on an atomic (`lib.rs`), the host lab
//! (`leviculum-heap-lab`) includes this same file and tests the routing
//! with threads.

use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::pool_tlsf::PoolTlsf;

/// What guards one pool: runs `f` with no other caller of the same lock
/// inside, on any core or in any interrupt.
pub trait PoolLock {
    fn with<R>(&self, f: impl FnOnce() -> R) -> R;
}

/// One heap, its lock and the region it was placed in.
pub struct Pool<L> {
    lock: L,
    heap: UnsafeCell<Option<PoolTlsf>>,
    /// The region, `[start, end)`; both 0 while the pool is not placed.
    /// Written once under the lock, before any block of it exists, so a
    /// free that reads them unlocked reads them placed.
    start: AtomicUsize,
    end: AtomicUsize,
}

// SAFETY: the heap is reached only inside `lock.with`, which admits one
// caller at a time; the region bounds are atomics.
unsafe impl<L: PoolLock + Sync> Sync for Pool<L> {}

impl<L: PoolLock> Pool<L> {
    /// A pool not yet placed, guarded by `lock`.
    pub const fn new(lock: L) -> Self {
        Self {
            lock,
            heap: UnsafeCell::new(None),
            start: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
        }
    }

    /// Whether `at` lies in this pool's region.
    pub fn holds(&self, at: usize) -> bool {
        at >= self.start.load(Ordering::Acquire) && at < self.end.load(Ordering::Acquire)
    }

    /// Whether the pool was placed.
    pub fn placed(&self) -> bool {
        self.end.load(Ordering::Acquire) != 0
    }

    /// Place the pool in the `size` bytes at `start`. False when it is
    /// placed already or the region cannot hold the control block and a
    /// pool ([`PoolTlsf::place`]).
    ///
    /// # Safety
    ///
    /// [`PoolTlsf::place`]'s: the region is mapped, writable, and used
    /// by nothing else (no other pool either) for as long as the image
    /// runs.
    pub unsafe fn place(&self, start: usize, size: usize) -> bool {
        self.lock.with(|| {
            // SAFETY: inside the lock, the only way to the heap.
            let heap = unsafe { &mut *self.heap.get() };
            if heap.is_some() {
                return false;
            }
            // SAFETY: the caller's contract.
            *heap = unsafe { PoolTlsf::place(start, size) };
            if heap.is_none() {
                return false;
            }
            // The whole region, control block included: no block lies
            // there, so the wider range routes nothing wrongly, and a
            // pointer is told by the region its caller placed.
            self.start.store(start, Ordering::Release);
            self.end
                .store(start.saturating_add(size), Ordering::Release);
            true
        })
    }

    /// A block for `layout` from this pool, `None` when it has none that
    /// fits or is not placed.
    pub fn allocate(&self, layout: Layout) -> Option<NonNull<u8>> {
        self.lock.with(|| {
            // SAFETY: inside the lock, the only way to the heap.
            unsafe { &mut *self.heap.get() }
                .as_mut()
                .and_then(|tlsf| tlsf.allocate(layout))
        })
    }

    /// Give back a block of this pool.
    ///
    /// # Safety
    ///
    /// `ptr` came from [`Self::allocate`] on this pool with alignment
    /// `align` and has not been given back since.
    pub unsafe fn deallocate(&self, ptr: NonNull<u8>, align: usize) {
        self.lock.with(|| {
            // SAFETY: inside the lock, the only way to the heap.
            if let Some(tlsf) = unsafe { &mut *self.heap.get() }.as_mut() {
                // SAFETY: the caller's contract.
                unsafe { tlsf.deallocate(ptr, align) };
            }
        });
    }

    /// The payload bytes of the pool's free blocks (a walk of the pool).
    pub fn free(&self) -> usize {
        self.lock.with(|| {
            // SAFETY: inside the lock, the only way to the heap.
            unsafe { &*self.heap.get() }
                .as_ref()
                .map_or(0, PoolTlsf::free)
        })
    }
}

/// `N` pools, one per core; pool 0 is also every core's fallback.
pub struct Pools<L, const N: usize> {
    pools: [Pool<L>; N],
}

impl<L: PoolLock, const N: usize> Pools<L, N> {
    pub const fn new(pools: [Pool<L>; N]) -> Self {
        Self { pools }
    }

    /// Pool `index`, or `None` past the last.
    pub fn pool(&self, index: usize) -> Option<&Pool<L>> {
        self.pools.get(index)
    }

    /// Place pool `index` in the `size` bytes at `start`; false for an
    /// index past the last and as [`Pool::place`].
    ///
    /// # Safety
    ///
    /// [`Pool::place`]'s.
    pub unsafe fn place(&self, index: usize, start: usize, size: usize) -> bool {
        // SAFETY: the caller's contract.
        self.pools
            .get(index)
            .is_some_and(|pool| unsafe { pool.place(start, size) })
    }

    /// A block for `layout`: from pool `home` when it has one, else from
    /// the others in order, so a full or absent pool never fails a
    /// request another pool can serve. With the pool that served it.
    pub fn allocate(&self, home: usize, layout: Layout) -> Option<(NonNull<u8>, usize)> {
        let home = home.min(N.saturating_sub(1));
        if let Some(ptr) = self.pools.get(home).and_then(|pool| pool.allocate(layout)) {
            return Some((ptr, home));
        }
        self.pools
            .iter()
            .enumerate()
            .filter(|(index, pool)| *index != home && pool.placed())
            .find_map(|(index, pool)| pool.allocate(layout).map(|ptr| (ptr, index)))
    }

    /// The pool whose region holds `ptr`.
    pub fn owner(&self, ptr: *const u8) -> Option<usize> {
        let at = ptr as usize;
        self.pools.iter().position(|pool| pool.holds(at))
    }

    /// Give back a block to the pool that made it, whoever frees it. A
    /// pointer no pool holds is not ours and is left alone.
    ///
    /// # Safety
    ///
    /// `ptr` came from [`Self::allocate`] with alignment `align` and has
    /// not been given back since.
    pub unsafe fn deallocate(&self, ptr: NonNull<u8>, align: usize) {
        if let Some(index) = self.owner(ptr.as_ptr()) {
            // SAFETY: the block lies in that pool's region, so that pool
            // made it (the regions are disjoint): the caller's contract.
            unsafe { self.pools[index].deallocate(ptr, align) };
        }
    }

    /// The free bytes of all pools.
    pub fn free(&self) -> usize {
        self.pools.iter().map(Pool::free).sum()
    }
}
