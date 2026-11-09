//! The M9's per-core heap pools (order 1594, `leviculum-esp/src/pools.rs`)
//! on the host: an allocation lands in the asking core's pool, a free
//! goes to the pool that made the block whoever frees it, a full or
//! absent pool hands the request on, and two threads standing in for the
//! two cores, each freeing the other's blocks, leave both pools whole.

use core::alloc::Layout;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use leviculum_heap_lab::pools::{Pool, PoolLock, Pools};

/// The host's stand-in for the firmware's lock: a spin on an atomic.
struct Spin(AtomicBool);

impl PoolLock for Spin {
    fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        while self
            .0
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        let out = f();
        self.0.store(false, Ordering::Release);
        out
    }
}

fn pools() -> &'static Pools<Spin, 2> {
    Box::leak(Box::new(Pools::new([
        Pool::new(Spin(AtomicBool::new(false))),
        Pool::new(Spin(AtomicBool::new(false))),
    ])))
}

/// A leaked, `u64`-aligned region of `bytes`, never freed: the pools
/// hold it for the rest of the test process.
fn region(bytes: usize) -> usize {
    Box::leak(vec![0u64; bytes / 8].into_boxed_slice()).as_mut_ptr() as usize
}

fn placed(io: usize, ui: usize) -> &'static Pools<Spin, 2> {
    let pools = pools();
    // SAFETY: fresh leaked regions, each given to one pool only.
    unsafe {
        assert!(pools.place(0, region(io), io));
        if ui > 0 {
            assert!(pools.place(1, region(ui), ui));
        }
    }
    pools
}

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size, 8).expect("a small power-of-two alignment")
}

#[test]
fn an_allocation_lands_in_the_asking_cores_pool() {
    let pools = placed(1 << 20, 1 << 20);
    let (a, from_a) = pools.allocate(0, layout(100)).expect("pool 0 has room");
    let (b, from_b) = pools.allocate(1, layout(100)).expect("pool 1 has room");
    assert_eq!((from_a, from_b), (0, 1));
    assert_eq!(pools.owner(a.as_ptr()), Some(0));
    assert_eq!(pools.owner(b.as_ptr()), Some(1));
    // SAFETY: both blocks came from these pools with alignment 8.
    unsafe {
        pools.deallocate(a, 8);
        pools.deallocate(b, 8);
    }
}

#[test]
fn a_free_goes_back_to_the_pool_that_made_the_block() {
    let pools = placed(1 << 20, 1 << 20);
    let ui = pools.pool(1).expect("two pools");
    let io = pools.pool(0).expect("two pools");
    let (ui_free, io_free) = (ui.free(), io.free());
    let (block, from) = pools.allocate(1, layout(4096)).expect("room");
    assert_eq!(from, 1);
    assert!(ui.free() < ui_free);
    assert_eq!(io.free(), io_free, "pool 0 untouched by core 1's block");
    // Freed by "core 0": the routing is by address, not by the caller.
    let at = block.as_ptr() as usize;
    let freed = std::thread::spawn(move || {
        let block = core::ptr::NonNull::new(at as *mut u8).expect("non-null");
        // SAFETY: from pool 1 with alignment 8, given back once.
        unsafe { pools.deallocate(block, 8) };
    });
    freed.join().expect("the freeing thread");
    assert_eq!((ui.free(), io.free()), (ui_free, io_free));
}

#[test]
fn a_full_pool_hands_the_request_to_the_shared_one() {
    // Pool 1 smaller than the request: pool 0 serves it.
    // (A 64-bit host's control block alone is about 33 KiB.)
    let pools = placed(1 << 20, 64 << 10);
    let (big, from) = pools
        .allocate(1, layout(256 << 10))
        .expect("pool 0 serves it");
    assert_eq!(from, 0);
    assert_eq!(pools.owner(big.as_ptr()), Some(0));
    // SAFETY: from pool 0, alignment 8.
    unsafe { pools.deallocate(big, 8) };
}

#[test]
fn a_core_without_a_pool_allocates_from_the_shared_one() {
    // Before the UI core's pool is placed (the boot's first allocations).
    let pools = placed(1 << 20, 0);
    let (block, from) = pools.allocate(1, layout(64)).expect("pool 0 serves it");
    assert_eq!(from, 0);
    // SAFETY: from pool 0, alignment 8.
    unsafe { pools.deallocate(block, 8) };
    // A home past the last pool is the last pool, not a panic.
    let (block, _) = pools.allocate(7, layout(64)).expect("served");
    // SAFETY: as above.
    unsafe { pools.deallocate(block, 8) };
}

#[test]
fn a_pointer_no_pool_holds_is_left_alone() {
    let pools = placed(64 << 10, 64 << 10);
    let free = pools.free();
    let mut outside = 0u64;
    let ptr = core::ptr::NonNull::from(&mut outside).cast::<u8>();
    assert_eq!(pools.owner(ptr.as_ptr()), None);
    // SAFETY: the contract is broken on purpose: no pool holds it, so
    // `deallocate` must not reach any heap.
    unsafe { pools.deallocate(ptr, 8) };
    assert_eq!(pools.free(), free);
}

#[test]
fn a_pool_is_placed_once() {
    let pools = placed(64 << 10, 64 << 10);
    // SAFETY: a fresh region, refused, so never used.
    assert!(!unsafe { pools.place(1, region(64 << 10), 64 << 10) });
    assert!(!unsafe { pools.place(2, region(64 << 10), 64 << 10) });
}

/// Two threads as the two cores: each allocates in its own pool and
/// hands every other block to the other to free, the M9's queues' way
/// (core 0 makes a message, the UI core drops it, and back). Both pools
/// end as they began, and no block was handed out twice (each block is
/// stamped with its owner's number and read back before it is freed).
#[test]
fn two_cores_freeing_each_others_blocks_leave_both_pools_whole() {
    const ROUNDS: usize = 20_000;
    let pools = placed(4 << 20, 4 << 20);
    let before = [
        pools.pool(0).expect("pool").free(),
        pools.pool(1).expect("pool").free(),
    ];
    let (to_1, from_0) = mpsc::channel::<(usize, usize, u8)>();
    let (to_0, from_1) = mpsc::channel::<(usize, usize, u8)>();
    let core = move |home: usize,
                     give: mpsc::Sender<(usize, usize, u8)>,
                     take: mpsc::Receiver<(usize, usize, u8)>| {
        let mut kept = Vec::new();
        for n in 0..ROUNDS {
            let size = 16 + (n * 37) % 700;
            let (ptr, from) = pools.allocate(home, layout(size)).expect("room");
            assert_eq!(from, home, "a pool this size never falls back");
            let stamp = (n % 251) as u8;
            // SAFETY: a fresh block of `size` bytes.
            unsafe { ptr.as_ptr().write_bytes(stamp, size) };
            let block = (ptr.as_ptr() as usize, size, stamp);
            match n % 2 {
                0 => give.send(block).expect("the other core"),
                _ => kept.push(block),
            }
            if kept.len() > 64 {
                free(pools, kept.remove(0));
            }
            while let Ok(theirs) = take.try_recv() {
                free(pools, theirs);
            }
        }
        drop(give);
        for block in kept {
            free(pools, block);
        }
        for theirs in take {
            free(pools, theirs);
        }
    };
    let a = std::thread::spawn(move || core(0, to_1, from_1));
    let b = std::thread::spawn(move || core(1, to_0, from_0));
    a.join().expect("core 0");
    b.join().expect("core 1");
    let after = [
        pools.pool(0).expect("pool").free(),
        pools.pool(1).expect("pool").free(),
    ];
    assert_eq!(after, before);
}

/// Check a block's stamp, then give it back.
fn free(pools: &Pools<Spin, 2>, (at, size, stamp): (usize, usize, u8)) {
    // SAFETY: a live block of `size` bytes, stamped when it was made.
    let bytes = unsafe { core::slice::from_raw_parts(at as *const u8, size) };
    assert!(
        bytes.iter().all(|&b| b == stamp),
        "a block was handed out twice"
    );
    let ptr = core::ptr::NonNull::new(at as *mut u8).expect("non-null");
    // SAFETY: from these pools, alignment 8, given back once.
    unsafe { pools.deallocate(ptr, 8) };
}
