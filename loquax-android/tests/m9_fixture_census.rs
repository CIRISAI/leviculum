//! The census the M9's image prints at boot (slice four, 1361), run on the
//! host over the same fixture model and held to the same budgets
//! ([`loquax_m9::census`]), and the client session's pull (slice five,
//! 1363). Its own test binary, its two tests one at a time ([`SERIAL`]),
//! so nothing else allocates while the allocator counts; here beside
//! `m9_scene_census.rs` and not in `loquax-m9` for that file's reason: a
//! counting allocator is `unsafe` code, which `loquax-m9` forbids.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use loquax::home::ask::LORA_PAGE_BYTES;
use loquax::home::wire;
use loquax_m9::census::{self, Census, Probe, Step};
use loquax_m9::fixture::Screen;
use loquax_m9::frame::Frame;
use loquax_m9::render::Renderer;

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

// The one unsafe here: a GlobalAlloc that counts and hands every call to
// the system's, as `loquax-android/tests/m9_scene_census.rs` does.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live =
            LIVE.fetch_add(layout.size() as isize, Ordering::SeqCst) + layout.size() as isize;
        PEAK.fetch_max(live, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let delta = new as isize - layout.size() as isize;
        let live = LIVE.fetch_add(delta, Ordering::SeqCst) + delta;
        PEAK.fetch_max(live, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// One census at a time: the counts are the process's.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The stack a step may take, painted below the caller before it runs.
const PAINT_LEN: usize = 1 << 20;
const PAINT_BYTE: u8 = 0xA5;

#[inline(never)]
fn paint() -> usize {
    let mut area = [PAINT_BYTE; PAINT_LEN];
    std::hint::black_box(&mut area);
    area.as_ptr() as usize
}

/// The host's half of the census: the counting allocator and a painted
/// stack.
struct HostProbe {
    before: isize,
    low: usize,
}

impl Probe for HostProbe {
    #[inline(never)]
    fn begin(&mut self) {
        self.before = LIVE.load(Ordering::SeqCst);
        PEAK.store(self.before, Ordering::SeqCst);
        self.low = paint();
    }

    #[inline(never)]
    fn end(&mut self) -> Step {
        // The other unsafe: reading back the stack below the live frames,
        // which this thread owns and nothing else writes.
        #[allow(unsafe_code)]
        let byte = |at: usize| unsafe { std::ptr::read_volatile(at as *const u8) };
        let untouched = (0..PAINT_LEN)
            .position(|n| byte(self.low + n) != PAINT_BYTE)
            .unwrap_or(PAINT_LEN);
        Step {
            held: LIVE.load(Ordering::SeqCst) - self.before,
            peak: (PEAK.load(Ordering::SeqCst) - self.before).max(0) as usize,
            stack: PAINT_LEN - untouched,
        }
    }
}

/// The census the board prints, over the same model, held to the same
/// budgets.
#[test]
fn the_two_screens_keep_the_boards_budgets() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut probe = HostProbe { before: 0, low: 0 };
    let mut panel = Frame::new();
    let before = LIVE.load(Ordering::SeqCst);
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    let renderer_held = LIVE.load(Ordering::SeqCst) - before;
    println!("renderer made: held {renderer_held} B");
    assert!((renderer_held as usize) < census::RENDERER_HELD);
    for (screen, budget) in [(Screen::Chats, census::CHATS), (Screen::Chat, census::CHAT)] {
        let model = screen.model();
        let got: Census = census::measure(&mut probe, &model, &mut renderer).expect("draw");
        let line = census::Line {
            screen: screen.name(),
            census: &got,
            budget: &budget,
        };
        println!("[CENSUS] {line}");
        assert!(
            got.scene.peak > 0 && got.scene.stack > 0,
            "{screen:?}: the probe measured nothing"
        );
        assert_eq!(budget.over(&got), None, "{line}");
    }
}

/// Archive entries as a home keeps them: LXMF messages of `len` bytes.
fn entries(n: u64, len: usize) -> Vec<wire::Entry> {
    (1..=n)
        .map(|seq| wire::Entry {
            seq,
            kind: loquax::home::KIND_RECEIVED,
            at: 1_793_600_000,
            packed: vec![seq as u8; len],
        })
        .collect()
}

/// The answers a home gives a client that asks LoRa pages, walked by
/// cursor as the home walks them (`wire::fit_page`).
fn lora_answers(all: &[wire::Entry]) -> Vec<Vec<u8>> {
    let mut answers = Vec::new();
    let mut at = 0;
    while at < all.len() {
        let mut page = wire::Page {
            entries: all[at..].to_vec(),
            more: false,
            first: 1,
        };
        wire::fit_page(&mut page, LORA_PAGE_BYTES);
        at += page.entries.len();
        answers.push(wire::sync_answer(&page));
    }
    answers
}

/// The client session's pull (1363): thirty messages of 200 bytes, a
/// typical text with its signature and fields, in LoRa pages; then the
/// same archive as one page of the home's own bound, the positive
/// control, which the ceiling refuses.
#[test]
fn a_client_sessions_pull_keeps_its_budget_in_lora_pages() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut probe = HostProbe { before: 0, low: 0 };
    let home = leviculum_core::Identity::generate(&mut rand_core::OsRng).public_key_bytes();
    let all = entries(30, 200);
    let answers = lora_answers(&all);
    assert!(answers.iter().all(|answer| answer.len() <= LORA_PAGE_BYTES));
    let got = census::session(&mut probe, &home, &answers).expect("session");
    let line = census::SessionLine(&got);
    println!(
        "[CENSUS] {line} pages={} entries={}",
        answers.len(),
        all.len()
    );
    assert!(got.peak > 0 && got.stack > 0, "the probe measured nothing");
    assert_eq!(line.over(), None, "{line}");

    // Control: the home's own page, 64 KiB of these, in one answer.
    let all = entries(300, 200);
    let whole = vec![wire::sync_answer(&wire::Page {
        entries: all,
        more: false,
        first: 1,
    })];
    let got = census::session(&mut probe, &home, &whole).expect("session");
    let line = census::SessionLine(&got);
    println!("[CENSUS] control {line} answer={} B", whole[0].len());
    assert_eq!(line.over(), Some("session_peak"), "{line}");
}
