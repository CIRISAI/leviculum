//! A list row's name questions cost the same whatever the number of names
//! (1581, #474): counted in allocator calls, never timed. Window 17's
//! board spent four seconds a draw in `Names::collisions`, which built the
//! folded name of every other named peer for each question, and a row asks
//! twice (`Names::label` for its name, `Names::same_name` for its "two
//! contacts are called" sentence). Red at the commit before 1581: a row's
//! calls grew by about two per named peer.
//!
//! One test in its own binary, on the test's own thread: it lives here
//! because a counting allocator is `unsafe` code, which `loquax` forbids
//! and this crate allows where it is named (as `m9_fold_census.rs`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use loquax::names::Names;
use loquax::ui::strings::Lang;

/// The system allocator, counting the calls made on this thread while
/// `COUNTING` is set, so the harness's own threads count nothing.
struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

// The one unsafe here: a GlobalAlloc that counts and hands every call to
// the system's.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNTING.try_with(|on| {
            if on.get() {
                let _ = CALLS.try_with(|c| c.set(c.get() + 1));
            }
        });
        // SAFETY: the caller's layout, handed to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The allocator calls `f` makes on this thread.
fn calls(f: impl FnOnce()) -> u64 {
    CALLS.with(|c| c.set(0));
    COUNTING.with(|on| on.set(true));
    f();
    COUNTING.with(|on| on.set(false));
    CALLS.with(Cell::get)
}

fn peer(i: usize) -> [u8; 16] {
    let mut peer = [0x50; 16];
    peer[..8].copy_from_slice(&(i as u64).to_be_bytes());
    peer
}

/// `n` peers with a conversation each, all named apart, and two more
/// who share a name.
fn names(n: usize) -> Names {
    let mut names = Names::default();
    for i in 0..n {
        names.heard(peer(i), Some(&format!("Peer {i}")));
        names.meet(peer(i));
    }
    names.heard(peer(n), Some("Twin"));
    names.heard(peer(n + 1), Some("twin"));
    names
}

/// What a row asks of the names, after the index is built once.
fn row_calls(names: &Names, who: &[u8; 16]) -> u64 {
    let _ = names.same_name(who);
    calls(|| {
        std::hint::black_box(names.label(who, Lang::En));
        std::hint::black_box(names.same_name(who));
    })
}

#[test]
fn a_rows_name_questions_cost_the_same_for_32_names_and_for_1000() {
    let (few, many) = (names(32), names(1000));
    for who in [peer(7), peer(9_999)] {
        let (a, b) = (row_calls(&few, &who), row_calls(&many, &who));
        assert_eq!(
            a, b,
            "a row's allocator calls with 32 names {a}, with 1000 {b}"
        );
        assert!(b <= 2, "a row's name and sentence: {b} allocator calls");
    }
    // A collision's row too: its tail and its sentence, never a walk.
    let (a, b) = (row_calls(&few, &peer(32)), row_calls(&many, &peer(1000)));
    assert_eq!(a, b, "a twin's row: with 32 names {a}, with 1000 {b}");
    assert!(b <= 16, "a twin's row: {b} allocator calls");
}
