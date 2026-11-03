//! The census the M9's image prints at boot (slice four, 1361), run on the
//! host over the same fixture model and held to the same budgets
//! ([`loquax_m9::census`]). Its own test binary, with one test in it, so
//! nothing else allocates while the allocator counts; here beside
//! `m9_scene_census.rs` and not in `loquax-m9` for that file's reason: a
//! counting allocator is `unsafe` code, which `loquax-m9` forbids.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

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
