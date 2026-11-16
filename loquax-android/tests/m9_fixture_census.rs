//! The census the M9's image prints at boot (slice four, 1361), run on the
//! host over the same fixture model and held to the same budgets
//! ([`loquax_m9::census`]), and the client session's pull (slice five,
//! 1363). Its own test binary, its tests one at a time ([`SERIAL`]), and
//! the allocator counts each thread on its own ([`LIVE`]), so no other
//! thread's bytes land in a census; here beside
//! `m9_scene_census.rs` and not in `loquax-m9` for that file's reason: a
//! counting allocator is `unsafe` code, which `loquax-m9` forbids.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use loquax::home::ask::LORA_PAGE_BYTES;
use loquax::home::wire;
use loquax_m9::census::{self, Census, Probe, Step};
use loquax_m9::fixture::SCREENS;
use loquax_m9::font::Size;
use loquax_m9::frame::Frame;
use loquax_m9::map::{Map, FIXTURE};
use loquax_m9::render::Renderer;

struct Counting;

std::thread_local! {
    /// The bytes this thread holds, and the most it held since the last
    /// [`Probe::begin`] on it. Each thread counts its own and a census
    /// reads its own, so no other thread's allocation can land in it.
    ///
    /// The first answer was one process-wide count ([`SERIAL`] alone),
    /// which took libtest's main thread's bytes as the census's about one
    /// run in twenty: the session pull read `held=522`, then `held=1431`.
    /// The second counted only threads that had taken a census, but such
    /// a thread stays one after its test body returns, and libtest's
    /// epilogue on it (the copy of the captured stdout, the result it
    /// sends) ran inside the next census's window on the two-core forge:
    /// the land gate on 902bc129 read the pull at `held=8155 peak=11160`
    /// against `held=0 peak=392` everywhere else (1583,
    /// [`a_census_counts_no_other_threads_bytes`]).
    ///
    /// Const-initialised and without a destructor, so reading them from
    /// inside the allocator allocates nothing and works while the thread
    /// is torn down.
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn live() -> isize {
    LIVE.with(Cell::get)
}

fn add(delta: isize) {
    let now = live() + delta;
    LIVE.with(|live| live.set(now));
    PEAK.with(|peak| peak.set(peak.get().max(now)));
}

// The one unsafe here: a GlobalAlloc that counts and hands every call to
// the system's, as `loquax-android/tests/m9_scene_census.rs` does.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size() as isize);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        add(-(layout.size() as isize));
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        add(new as isize - layout.size() as isize);
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
        self.before = live();
        PEAK.with(|peak| peak.set(self.before));
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
            held: live() - self.before,
            peak: (PEAK.with(Cell::get) - self.before).max(0) as usize,
            stack: PAINT_LEN - untouched,
        }
    }
}

/// The census the board prints, over the same models, held to the same
/// budgets: the two screens of slice four, the group's seven of slice
/// six (1366) and the map's two of slice seven (1375), in both type sizes
/// (1532), the large one's lines named `<screen>-large`.
#[test]
fn every_screen_keeps_the_boards_budgets() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut probe = HostProbe { before: 0, low: 0 };
    let mut panel = Frame::new();
    let before = live();
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    let renderer_held = live() - before;
    println!("renderer made: held {renderer_held} B");
    assert!((renderer_held as usize) < census::RENDERER_HELD);
    let before = live();
    let mut map = Map::new(FIXTURE).expect("the fixture region");
    let painter_held = live() - before;
    println!(
        "map made: held {painter_held} B, the painter's {} B",
        map.painter.bytes()
    );
    assert!((painter_held as usize) < census::PAINTER_HELD);
    for size in Size::ALL {
        for screen in SCREENS {
            let budget = screen.budget();
            let (mut model, row) = screen.walk();
            let _ = loquax_m9::house::set_size(size, &mut model, &mut renderer);
            let got: Census =
                census::measure_at(&mut probe, &model, row, &mut renderer, &mut map).expect("draw");
            let name = census::screen_name(screen.name(), size);
            let line = census::Line {
                screen: &name,
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
    let all = census::archive(30, 200);
    let answers = census::lora_answers(&all);
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
    // The batch the board runs with on this census (1414): the LoRa page.
    assert_eq!(
        census::batch(&got, 64 * 1024).page_bytes,
        LORA_PAGE_BYTES as u64
    );

    // Control: the home's own page, 64 KiB of these, in one answer.
    let all = census::archive(300, 200);
    let whole = vec![wire::sync_answer(&wire::Page {
        entries: all,
        more: false,
        first: 1,
        left: None,
    })];
    let got = census::session(&mut probe, &home, &whole).expect("session");
    let line = census::SessionLine(&got);
    println!("[CENSUS] control {line} answer={} B", whole[0].len());
    assert_eq!(line.over(), Some("session_peak"), "{line}");
    // A board that measured this would ask the floor's page, not panic.
    assert_eq!(
        census::batch(&got, 64 * 1024).page_bytes,
        loquax::home::board::MIN_PAGE_BYTES
    );
}

/// What another thread allocates while a census is taken is not the
/// census's (1583). The land gate on 902bc129 read the pull at
/// `held=8155 peak=11160` where every host before and since reads
/// `held=0 peak=392`: the screens' test had taken its census on its own
/// thread, which stayed counted after its body returned and the lock
/// went to the pull, and libtest's epilogue on that thread (the copy of
/// its captured stdout, the result it sends the main thread) landed
/// inside the pull's window on a two-core forge. Here the other thread
/// allocates between [`Probe::begin`] and [`Probe::end`],
/// deterministically, on a barrier that allocates nothing; with one
/// process-wide count, or one shared by every census thread, this read
/// `peak=8192`.
#[test]
fn a_census_counts_no_other_threads_bytes() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let other = std::thread::spawn({
        let gate = gate.clone();
        move || {
            gate.wait();
            let bytes = std::hint::black_box(vec![0u8; 8 * 1024]);
            gate.wait();
            drop(bytes);
        }
    });
    let mut probe = HostProbe { before: 0, low: 0 };
    probe.begin();
    gate.wait();
    gate.wait();
    let got = probe.end();
    other.join().expect("the other thread");
    assert_eq!(
        (got.held, got.peak),
        (0, 0),
        "another thread's 8 KiB were counted as the census's"
    );
}
