//! 1575: where a pulled page's fold goes on the M9's UI core, counted on
//! the host. Window 16b (2026-11-08) saw drain turns of 836 ms folding
//! 234 entries and 536 to 620 ms folding 55: a page costs far more than
//! its entries, and the host folds one entry in 12 to 17 us. This test
//! folds pages of both sizes into a model shaped like the 1489 fixture
//! (300 conversations, about 4000 messages) and splits each fold into
//! the entries' unpack (`board::fold`), the model's step per entry, the
//! batch's note at its end and the route of what it left (`home::route`,
//! the rows for the summary log), each by time, by allocator calls and
//! by bytes allocated: the board's allocator is the share the host's is
//! not (alloc_ms1 80 to 429 ms per 5 s in window 16b). Then the same
//! page folded a budget at a time (`home::Folding`), to show what a
//! split costs: each chunk is a batch of its own, noted and routed once,
//! and since 1591 that costs a few allocator calls a chunk more than the
//! whole page's note and route (the note builds no scene; the frame's
//! read does), not the list's window again per chunk.
//!
//! One test in its own binary: the allocator counts the whole process.
//! It lives here because a counting allocator is `unsafe` code, which
//! `loquax-m9` forbids and this crate allows where it is named.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::home::{self, Folding, TakeSpent};
use rand_core::OsRng;

struct Counting;

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

// The one unsafe here: a GlobalAlloc that counts and hands every call to
// the system's.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Ordering::SeqCst);
        BYTES.fetch_add(layout.size() as u64, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        CALLS.fetch_add(1, Ordering::SeqCst);
        BYTES.fetch_add(new as u64, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const CONVERSATIONS: usize = 300;

struct Archive {
    me: Identity,
    address: [u8; 16],
    friends: Vec<Identity>,
}

impl Archive {
    fn new() -> Self {
        let me = Identity::generate(&mut OsRng);
        let address = lxmf_address(&me.public_key_bytes()).expect("address");
        let friends = (0..CONVERSATIONS)
            .map(|_| Identity::generate(&mut OsRng))
            .collect();
        Self {
            me,
            address,
            friends,
        }
    }

    /// Entries `from..to`, the 1489 fixture's shape: two long chats,
    /// twenty-three friends with about 40 each, the rest a few each; one
    /// in five ours.
    fn entries(&self, from: u64, to: u64) -> Vec<Entry> {
        (from..to)
            .map(|seq| {
                let at = (seq - 1) as usize;
                let friend = &self.friends[match at % 4000 {
                    a @ 0..1700 => a % 2,
                    a @ 1700..2620 => 2 + (a - 1700) % 23,
                    a => 25 + (a - 2620) % (CONVERSATIONS - 25),
                }];
                let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
                let at = 1_762_000_000.0 + seq as f64;
                let (kind, from, to, body) = match seq % 5 {
                    0 => (KIND_SENT, &self.me, theirs, format!("Antwort {seq}")),
                    _ => (
                        KIND_RECEIVED,
                        friend,
                        self.address,
                        format!("Nachricht {seq}"),
                    ),
                };
                let source = lxmf_address(&from.public_key_bytes()).expect("address");
                let packed = Message::create(
                    to,
                    source,
                    from,
                    at,
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
                    at: at as i64,
                    packed,
                }
            })
            .collect()
    }
}

fn micros() -> u64 {
    thread_local!(static START: Instant = Instant::now());
    START.with(|start| start.elapsed().as_micros() as u64)
}

fn calls() -> u64 {
    CALLS.load(Ordering::SeqCst)
}

fn bytes() -> u64 {
    BYTES.load(Ordering::SeqCst)
}

/// One page folded and routed, its phases by `clock`: unpack, step,
/// finish, route.
fn phases(
    model: &mut UiModel<Field>,
    page: &[Entry],
    address: &[u8; 16],
    clock: &dyn Fn() -> u64,
) -> [u64; 4] {
    let (_, effects, spent) = home::take_timed(model, page, address, clock);
    let t0 = clock();
    let routed = home::route(model, effects, &mut || [7; 16]);
    let route = clock() - t0;
    std::hint::black_box(routed);
    [spent.unpack, spent.step, spent.finish, route]
}

/// The page folded `chunk` entries at a time through [`Folding`], each
/// chunk routed: the chunks, and the finish and route summed by `clock`.
fn chunked(
    model: &mut UiModel<Field>,
    page: Vec<Entry>,
    address: [u8; 16],
    chunk: usize,
    clock: &dyn Fn() -> u64,
) -> (usize, u64, u64) {
    let mut folding = Folding::default();
    folding.begin(page, address);
    let (mut chunks, mut route) = (0, 0);
    while folding.busy() {
        let mut n = 0;
        let out = folding.fold(model, clock, u64::MAX, &mut || {
            n += 1;
            n >= chunk
        });
        // As the board routes a chunk (1591): its rows only after the
        // page's last.
        let t0 = clock();
        let routed = match out.done.is_some() {
            true => home::route(model, out.effects, &mut || [7; 16]),
            false => home::route_held(model, out.effects, &mut || [7; 16]),
        };
        std::hint::black_box(routed);
        route += clock() - t0;
        chunks += 1;
    }
    let (spent, _): (TakeSpent, usize) = folding.take_spent();
    (chunks, spent.finish, route)
}

#[test]
fn a_pulled_pages_fold_split_by_phase() {
    let archive = Archive::new();
    let all = archive.entries(1, 4000 + 8 * 234 + 1);
    let mut model = home::model(archive.address);
    for page in all[..3766].chunks(234) {
        home::take(&mut model, page, &archive.address);
    }
    let mut pages = all[3766..].chunks(234);
    let mut next = || pages.next().expect("a page").to_vec();

    let names = ["unpack", "step", "finish", "route"];
    let say = |what: &str, n: usize, v: [u64; 4]| {
        let per = |x: u64| x as f64 / n as f64;
        println!(
            "FOLD {what} entries={n} {}",
            names
                .iter()
                .zip(v)
                .map(|(name, x)| format!("{name}={x} ({:.1}/entry)", per(x)))
                .collect::<Vec<_>>()
                .join(" ")
        );
    };
    // The 16b shapes: a page of 234 and one of 55.
    let page = next();
    say(
        "us",
        234,
        phases(&mut model, &page, &archive.address, &micros),
    );
    let page = next();
    let calls_234 = phases(&mut model, &page, &archive.address, &calls);
    say("alloc_calls", 234, calls_234);
    let page = next();
    say(
        "alloc_bytes",
        234,
        phases(&mut model, &page, &archive.address, &bytes),
    );
    let page = next();
    let calls_55 = phases(&mut model, &page[..55], &archive.address, &calls);
    say("alloc_calls", 55, calls_55);
    say(
        "us",
        55,
        phases(&mut model, &page[55..110], &archive.address, &micros),
    );

    // The note and the route are per batch, not per entry: a page of 55
    // costs them as much as one of 234.
    let fixed = |v: [u64; 4]| v[2] + v[3];
    println!(
        "FOLD per_batch alloc_calls finish+route 234={} 55={}",
        fixed(calls_234),
        fixed(calls_55)
    );

    // The same page folded in chunks of 20 (the budget's split): each
    // chunk's note and route again.
    let page = next();
    let (chunks, finish, route) = chunked(&mut model, page, archive.address, 20, &calls);
    println!("FOLD chunked entries=234 chunk=20 chunks={chunks} alloc_calls finish={finish} route={route}");
    assert_eq!(chunks, 12, "234 entries in chunks of 20");
    // The split's price, by the allocator's calls (1591): the note
    // builds no scene since 1591 (the frame's read does), keeps its
    // bumped list between notes and its poked peers in the fold's own
    // sets, and a chunk that is not the page's last routes no rows
    // (`home::route_held`). What is left per chunk is a few calls: the
    // chunk's change set handed to the owed list and freed at the drain.
    // Measured 2026-11-09: 112 calls in twelve chunks against 70 whole,
    // 1.6x; before 1591, 5067 against 520 (9.7x), what window 18's
    // board paid as `finish` 9.2 of 16.3 ms per entry.
    assert!(
        finish + route <= 2 * fixed(calls_234),
        "chunks of 20: finish+route {} against {} whole, over 2x",
        finish + route,
        fixed(calls_234)
    );

    // And by time, against a whole page's (1591): the note's walk over
    // the conversations is per chunk, the rest per entry.
    let page = next();
    let whole = phases(&mut model, &page, &archive.address, &micros);
    let page = next();
    let (_, finish_us, route_us) = chunked(&mut model, page, archive.address, 20, &micros);
    println!(
        "FOLD chunked entries=234 chunk=20 us finish={finish_us} route={route_us} whole finish={} route={}",
        whole[2], whole[3]
    );
}
