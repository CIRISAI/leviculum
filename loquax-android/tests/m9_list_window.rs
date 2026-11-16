//! 1595: the M9's list draws the rows on its screen, not the store.
//! Window 20 (2026-11-09) counted 5,600 to 5,725 allocator calls per
//! list draw on the board whatever the draw folded, about 19 per
//! conversation times 302. The mechanism, found here: the board's fold
//! hides the selection behind the list (`loquax_m9::home`,
//! `behind_the_list`) and gives it back outside `update`, so the stamp
//! the fold's note took never matched the draw's, and every draw of a
//! pull took the cold path `scene::list`, which built the whole order
//! (302 rows, 10,577 allocator calls on this host). This test pulls a
//! board's model of 302 conversations (and, as the control, of 30) page
//! by page as the board folds them, drawing after each page, then moves
//! the cursor down and up the list by the M9's keys and builds the scene
//! the board draws for each key, counting the rows the scene derived
//! ([`loquax::ui::counters::Counters::rows_built`]), the rows it carries
//! and the allocator calls it took.
//!
//! The bar: no more rows built per draw than the screen's rows plus one,
//! for 302 conversations as for 30, and the allocator calls per draw a
//! function of the rows built, not of the conversations held.
//!
//! One test in its own binary: the allocator counts the whole process.
//! It lives here because a counting allocator is `unsafe` code, which
//! `loquax-m9` forbids and this crate allows where it is named.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::{update, UiEvent, UiModel};
use loquax::when::WallTime;
use loquax_m9::field::Field;
use loquax_m9::group;
use loquax_m9::keys::{event, M9Key};
use loquax_m9::screen;
use rand_core::OsRng;

struct Counting;

static CALLS: AtomicU64 = AtomicU64::new(0);

// The one unsafe here: a GlobalAlloc that counts and hands every call to
// the system's.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The archive's first second.
const BASE: i64 = 1_762_000_000;

/// A board's page of pulled entries (window 16b: 234 a page).
const PAGE: usize = 234;

/// `conversations` friends writing three messages each, one in three
/// answered, interleaved so every page touches rows all over the list.
fn archive(conversations: usize) -> ([u8; 16], Vec<Entry>) {
    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..conversations)
        .map(|_| Identity::generate(&mut OsRng))
        .collect();
    let entries = (0..3 * conversations)
        .map(|at| {
            let seq = at as u64 + 1;
            let friend = &friends[at % conversations];
            let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
            let when = (BASE + at as i64 * 60) as f64;
            let (kind, from, to, body) = match at % 3 {
                2 => (KIND_SENT, &me, theirs, format!("Antwort {seq}")),
                _ => (KIND_RECEIVED, friend, address, format!("Nachricht {seq}")),
            };
            let source = lxmf_address(&from.public_key_bytes()).expect("address");
            let packed = Message::create(
                to,
                source,
                from,
                when,
                Vec::new(),
                body.into_bytes(),
                Vec::new(),
                DeliveryMethod::Direct,
            )
            .expect("create")
            .pack();
            Entry {
                seq,
                kind,
                at: when as i64,
                packed,
            }
        })
        .collect();
    (address, entries)
}

/// What the list's draws took, the most of any key.
#[derive(Debug, Default)]
struct Drawn {
    rows_built: u32,
    rows_carried: usize,
    allocs: u64,
}

/// A board's model of `conversations`, pulled; then forty keys down the
/// list and forty up, each answered by the scene the board draws.
fn walk(conversations: usize) -> (usize, Drawn) {
    let (own, entries) = archive(conversations);
    let mut model: UiModel<Field> = loquax_m9::home::model(own);
    update(
        &mut model,
        UiEvent::Clock(WallTime {
            unix_secs: BASE + 3 * conversations as i64 * 60 + 3600,
            offset_secs: 3600,
        }),
    );
    let mut most = Drawn::default();
    let look = |model: &UiModel<Field>, most: &mut Drawn| {
        model.reset_counters();
        let before = CALLS.load(Ordering::SeqCst);
        let scene = screen::scene(model);
        let allocs = CALLS.load(Ordering::SeqCst) - before;
        let built = model.counters().expect("a test build counts").rows_built;
        let carried = scene.list.as_ref().map_or(0, |l| l.rows.len());
        most.rows_built = most.rows_built.max(built);
        most.rows_carried = most.rows_carried.max(carried);
        most.allocs = most.allocs.max(allocs);
    };
    // The pull, a draw after each page as the board's loop draws.
    for page in entries.chunks(PAGE) {
        loquax_m9::home::take(&mut model, page, &own);
        look(&model, &mut most);
    }
    assert_eq!(model.listed().count(), conversations, "the pull lists all");
    let mut row = 0;
    for press in 0..80 {
        let key = match press < 40 {
            true => M9Key::Down,
            false => M9Key::Up,
        };
        group::press(&mut model, &mut row, &event(key));
        look(&model, &mut most);
    }
    (model.viewing.list_rows, most)
}

#[test]
fn a_draw_builds_the_rows_on_screen_not_the_list() {
    let (screen_rows, small) = walk(30);
    let (_, large) = walk(302);
    println!(
        "M9_LIST_WINDOW screen_rows={screen_rows} \
         n=30 rows_built={} rows_carried={} allocs={} \
         n=302 rows_built={} rows_carried={} allocs={}",
        small.rows_built,
        small.rows_carried,
        small.allocs,
        large.rows_built,
        large.rows_carried,
        large.allocs
    );
    let bar = screen_rows as u32 + 1;
    assert!(
        large.rows_built <= bar,
        "302 conversations: a draw built {} rows, the screen shows {screen_rows}",
        large.rows_built
    );
    assert!(
        large.rows_carried as u32 <= bar,
        "302 conversations: a draw's scene carries {} rows, the screen shows {screen_rows}",
        large.rows_carried
    );
    // The calls follow the rows built, not the conversations held: ten
    // times the conversations, the same screen, about the same calls (the
    // rows' words differ, an age in minutes or in hours; before 1595 the
    // pull's draws took 10,577 for 302 against 817 for 30), and a bounded
    // number per row built.
    assert!(
        large.allocs <= small.allocs * 3 / 2,
        "allocator calls per draw grew with the conversations: {} for 30, {} for 302",
        small.allocs,
        large.allocs
    );
    assert!(
        large.allocs <= 64 * u64::from(large.rows_built) + 64,
        "{} allocator calls for {} rows built",
        large.allocs,
        large.rows_built
    );
}
