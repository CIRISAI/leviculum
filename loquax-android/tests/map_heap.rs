//! The phone's map zoomed out to the world and back stays within memory
//! (1797). The render walk of 2026-11-13 saw the native heap of Loquax
//! jump from about 195 to 630 MB over zoom-out series on a 1224 x 2197
//! frame, and the low-memory killer end the app in the foreground.
//!
//! This binary counts every byte the facade allocates (leviculum-std's
//! counting shim) while it drives the map as `MapView` does: the exact
//! zoom kept by the caller, frames while the fingers move, a frame at
//! rest when they lift, Java's margins (none while the globe turns).
//! Measured on the host before the fix: each step out left 6 to 8 MB of
//! tiles in the tile cache until its 96 MB budget was full, and they
//! stayed after the view was back where it started; every world frame,
//! drawn whole, allocated a buffer of the frame's own size beside the
//! Bitmap and copied it over (10.7 MB for the turning globe, 24 MB for
//! Equal Earth with its margins).
//!
//! One test in the binary on purpose: the counter is process-wide, and a
//! second test running beside it would move it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use leviculum_map::blend::Blend;
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_std::heap_accounting::{live_bytes, CountingAllocator};
use loquax::when::WallTime;
use loquax_android::map::Canvas;
use loquax_android::schema::{MapRequest, MapShown};

/// The counting shim, and the highest live count since the last reset:
/// what a frame needs at its peak, not only what it leaves.
struct Peaked;

static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded to the counting shim with the layout it
// arrived with; the peak is a relaxed maximum beside it.
unsafe impl std::alloc::GlobalAlloc for Peaked {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: the caller's contract, passed on.
        let p = unsafe { CountingAllocator.alloc(layout) };
        PEAK.fetch_max(live_bytes(), Ordering::Relaxed);
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: the caller's contract, passed on.
        unsafe { CountingAllocator.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new: usize) -> *mut u8 {
        // SAFETY: the caller's contract, passed on.
        let p = unsafe { CountingAllocator.realloc(ptr, layout, new) };
        PEAK.fetch_max(live_bytes(), Ordering::Relaxed);
        p
    }
}

#[global_allocator]
static ALLOC: Peaked = Peaked;

/// Lew's phone under the bars, at its density.
const PHONE: (u32, u32) = (1224, 2197);
const DENSITY: u32 = 2750;

/// Out to the world and back this many times.
const CYCLES: usize = 3;

/// A pinch's step between two frames, in thousandths of a zoom step.
const PINCH_MILLI: u32 = 250;

/// What the view back where it started may hold beyond what it held
/// before the first zoom out: the tiles of the steps next to its own,
/// which the prefetch draws for the next pinch anyway (`tiles::plan`), a
/// frame's worth at most (9 x 14 tiles of 256 KiB for 1836 x 3295 with
/// the margins, 31.5 MiB), and what the world zooms placed once, their
/// names and the painters' buffers (10 MiB measured on 2026-11-13). Before
/// 1797 it was 80 MiB: the tiles of every step on the way, to the
/// cache's budget.
const RETURN_MARGIN: usize = 40 << 20;

/// What the heap besides the tiles may have grown by, back where it
/// started: the 10 MiB above and some room, never a cache by the back
/// door.
const OTHER_MARGIN: usize = 16 << 20;

/// What one more cycle out and back may add: nothing that stays.
const CYCLE_MARGIN: usize = 1 << 20;

/// A home with the Florence region, the autoload off: no test reaches for
/// the internet.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("home");
    let maps = home.path().join("maps");
    std::fs::create_dir_all(&maps).expect("maps");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr");
    std::fs::copy(fixture, maps.join("firenze.lmr")).expect("the region");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    loquax_android::map::hold_rung(home.path(), Some(0));
    home
}

fn now() -> WallTime {
    WallTime {
        unix_secs: 1_790_000_000,
        offset_secs: 0,
    }
}

/// A Bitmap as Java holds one: its size fixed, its pixels kept.
struct Bitmap {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

impl Bitmap {
    fn new((w, h): (u32, u32)) -> Self {
        Self {
            w,
            h,
            rgba: vec![0; w as usize * h as usize * 4],
        }
    }
}

impl Canvas for Bitmap {
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String> {
        if (w, h) != (self.w, self.h) {
            return Err(format!(
                "map: the bitmap is {}x{}, the frame {w}x{h}",
                self.w, self.h
            ));
        }
        Ok(&mut self.rgba)
    }
}

/// Java's frame for the last frame's `turns` (`MapFrame.margin`): the
/// screen and a quarter of it past each edge, none while the globe turns.
fn frame_size(turns: bool) -> (u32, u32) {
    let margin = |side: u32| match turns {
        true => 0,
        false => (f64::from(side) * 0.25).round() as u32,
    };
    (PHONE.0 + 2 * margin(PHONE.0), PHONE.1 + 2 * margin(PHONE.1))
}

/// The phone's map view: where it is, the exact zoom the fingers set,
/// the two Bitmaps it draws into (flat and turning), made before the
/// count starts as Java makes its own.
struct Phone {
    home: tempfile::TempDir,
    flat: Bitmap,
    globe: Bitmap,
    centre: World,
    zoom_milli: u32,
    turns: bool,
    floor_milli: u32,
}

/// One frame's cost: what it allocated at its peak beyond what was live
/// before it, and the bytes of its frame.
struct Cost {
    peak: usize,
    frame: usize,
    curved: bool,
}

impl Phone {
    fn new() -> Self {
        let at = project(LatLon::from_degrees(43.7752, 11.2660));
        Self {
            home: home(),
            flat: Bitmap::new(frame_size(false)),
            globe: Bitmap::new(frame_size(true)),
            centre: at,
            zoom_milli: 14_000,
            turns: false,
            floor_milli: 0,
        }
    }

    /// One frame at the view, moving or at rest; a frame at rest takes the
    /// view the library settled on, as Java does.
    fn frame(&mut self, moving: bool) -> (MapShown, Cost) {
        let (w, h) = frame_size(self.turns);
        let request = MapRequest {
            dir: self.home.path().display().to_string(),
            w: PHONE.0,
            h: PHONE.1,
            density_milli: DENSITY,
            x: Some(self.centre.x),
            y: Some(self.centre.y),
            zoom_milli: Some(self.zoom_milli),
            margin_x: (w - PHONE.0) / 2,
            margin_y: (h - PHONE.1) / 2,
            moving,
            ..MapRequest::default()
        };
        // Drawn whole when the blend holds at the screen (1710).
        let curved = Blend::new(
            loquax::picture::world_shape(),
            self.centre,
            f64::from(self.zoom_milli) / 1000.0,
            PHONE.0,
            PHONE.1,
        )
        .t() < 1.0;
        let bitmap = match self.turns {
            true => &mut self.globe,
            false => &mut self.flat,
        };
        let before = live_bytes();
        PEAK.store(before, Ordering::Relaxed);
        let shown = loquax_android::map::render_into(&request, Some(Vec::new()), now(), bitmap);
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(before);
        assert!(shown.ok, "z{}: {:?}", self.zoom_milli, shown.error);
        self.turns = shown.turns;
        self.floor_milli = shown.min_zoom_milli;
        if !moving {
            self.centre = World {
                x: shown.x,
                y: shown.y,
            };
            self.zoom_milli = shown.zoom_milli;
        }
        let cost = Cost {
            peak,
            frame: w as usize * h as usize * 4,
            curved,
        };
        (shown, cost)
    }

    /// A pinch to `to` (out to the floor at most), a frame each step while
    /// the fingers move, then one at rest; each frame's cost.
    fn pinch(&mut self, to: u32) -> Vec<(u32, Cost)> {
        let mut costs = Vec::new();
        loop {
            let to = to.max(self.floor_milli);
            self.zoom_milli = match to < self.zoom_milli {
                true => self.zoom_milli.saturating_sub(PINCH_MILLI).max(to),
                false => (self.zoom_milli + PINCH_MILLI).min(to),
            };
            let (_, cost) = self.frame(true);
            costs.push((self.zoom_milli, cost));
            if self.zoom_milli == to {
                break;
            }
        }
        let (shown, cost) = self.frame(false);
        costs.push((shown.zoom_milli, cost));
        costs
    }
}

#[test]
fn zooming_out_to_the_world_and_back_stays_within_memory() {
    let mut phone = Phone::new();
    // The view the user starts from, at rest and its picture complete.
    phone.frame(false);
    phone.frame(false);
    let tiles = |phone: &Phone| {
        loquax_android::map::tile_memory(phone.home.path()).map_or(0, |(_, bytes)| bytes)
    };
    let start = live_bytes();
    let start_other = start - tiles(&phone);
    let mut lines = vec![format!(
        "start: live {} KiB, tiles {} KiB",
        start >> 10,
        tiles(&phone) >> 10
    )];
    let mut first_back = None;
    let mut fails = Vec::new();
    for cycle in 0..CYCLES {
        for to in [0, 14_000] {
            for (zoom, cost) in phone.pinch(to) {
                lines.push(format!(
                    "cycle {cycle} z{zoom}: peak +{} KiB, frame {} KiB{}",
                    cost.peak >> 10,
                    cost.frame >> 10,
                    if cost.curved { ", drawn whole" } else { "" }
                ));
                // A world frame is drawn straight into the Bitmap: it
                // needs the outline's scratch, never a frame of its own.
                if cost.curved && cost.peak >= cost.frame / 2 {
                    fails.push(format!(
                        "cycle {cycle} z{zoom}: a world frame of {} KiB took {} KiB more at its peak",
                        cost.frame >> 10,
                        cost.peak >> 10
                    ));
                }
            }
        }
        let live = live_bytes();
        lines.push(format!(
            "cycle {cycle} back at z{}: live {} KiB, {:+} KiB on the start",
            phone.zoom_milli,
            live >> 10,
            (live as i64 - start as i64) >> 10
        ));
        let other = live - tiles(&phone);
        if other > start_other + OTHER_MARGIN {
            fails.push(format!(
                "cycle {cycle}: besides the tiles the heap grew by {} KiB, at most {} KiB allowed",
                (other - start_other) >> 10,
                OTHER_MARGIN >> 10
            ));
        }
        let first = *first_back.get_or_insert(live);
        if live > first + CYCLE_MARGIN {
            fails.push(format!(
                "cycle {cycle}: {} KiB more than after the first cycle",
                (live - first) >> 10
            ));
        }
        if live > start + RETURN_MARGIN {
            fails.push(format!(
                "cycle {cycle}: back at z{} the heap is {} KiB above the start, at most {} KiB allowed (tiles held: {:?})",
                phone.zoom_milli,
                (live - start) >> 10,
                RETURN_MARGIN >> 10,
                loquax_android::map::tile_memory(phone.home.path())
            ));
        }
    }
    assert!(
        fails.is_empty(),
        "{}\n\nevery frame:\n{}",
        fails.join("\n"),
        lines.join("\n")
    );
}
