//! The render walk's map half (1801): the phone's map drawn headless
//! through the facade's own call ([`loquax_android::map::render_into`])
//! into a Bitmap sized as `MapView.java` sizes it, over a zoom ladder at
//! fixed centres and frame sizes and through five gestures, every frame
//! checked by the rules of [`loquax_android::render_walk`].
//!
//! ```sh
//! cargo run --release -p loquax-android --example render_walk \
//!     [--features loquax/globe-world] -- --out DIR --variant NAME \
//!     [--quick | --selftest] [--inject FAULT]
//! ```
//!
//! Writes `DIR/rows-NAME.tsv` (one row a rule a frame or gesture),
//! `DIR/thumb/*.png` (every frame, at most 480 pixels high) and
//! `DIR/full/*.png` (a frame some rule failed on, whole).
//! `scripts/render-walk.py` runs it for both world projections and makes
//! the contact sheet; `just render-walk` runs that.
//!
//! `--inject FAULT` puts one fault into every frame or gesture, the
//! positive controls of the rules: `oversized-bitmap` (a Bitmap half again
//! the frame on each side, the 1796 fault), `blank-half` (the screen's
//! lower half undrawn), `stray-label` (a name across the right edge, on
//! the buttons, over another, of zoom 20), `error-text` (a notice),
//! `slow-frame` (20 ms more each moving frame), `heap-kept` (384 MB held
//! past a gesture's end) and `view-lost` (a rotation that moves the view).
//!
//! `RENDER_WALK_NAMES=<image>` prints the names of that picture with their
//! boxes, in the frame's pixels: a label verdict checked by eye.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use leviculum_map::labels::Rect;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::viewport::MILLI;
use leviculum_mapfill::style::MapTheme;
use loquax::mapframe::DrawnName;
use loquax::when::WallTime;
use loquax_android::map;
use loquax_android::render_walk::{
    frame_rules, java_margin, sequence_rules, FrameFacts, JavaBitmap, Played, Screen, Verdict,
    SENTINEL,
};
use loquax_android::schema::{MapRequest, MapShown};

/// The heap as the process holds it: live bytes and the most since the
/// last reset, the "native heap" of a gesture.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is the system allocator's with the same layout; the
// counters are only counted.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract, passed on.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { System.realloc(ptr, layout, new) };
        if !p.is_null() {
            let now = match new >= layout.size() {
                true => {
                    LIVE.fetch_add(new - layout.size(), Ordering::Relaxed) + new - layout.size()
                }
                false => {
                    LIVE.fetch_sub(layout.size() - new, Ordering::Relaxed) - (layout.size() - new)
                }
            };
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static HEAP: Counting = Counting;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

fn reset_peak() {
    PEAK.store(live(), Ordering::Relaxed);
}

/// A screen the walk draws for: Lew's phone, a tall phone, a tablet, the
/// window, and Lew's phone turned.
#[derive(Clone, Copy)]
struct Size {
    name: &'static str,
    w: u32,
    h: u32,
    density_milli: u32,
}

const SIZES: [Size; 5] = [
    Size {
        name: "1224x2197",
        w: 1224,
        h: 2197,
        density_milli: 2750,
    },
    Size {
        name: "1080x2400",
        w: 1080,
        h: 2400,
        density_milli: 2625,
    },
    Size {
        name: "1600x2560",
        w: 1600,
        h: 2560,
        density_milli: 2000,
    },
    Size {
        name: "900x560",
        w: 900,
        h: 560,
        density_milli: 1000,
    },
    Size {
        name: "2197x1224",
        w: 2197,
        h: 1224,
        density_milli: 2750,
    },
];

/// A fixed centre of the ladder.
#[derive(Clone, Copy)]
struct Centre {
    name: &'static str,
    lat: f64,
    lon: f64,
}

const CENTRES: [Centre; 7] = [
    Centre {
        name: "germany",
        lat: 51.0,
        lon: 10.0,
    },
    Centre {
        name: "morocco",
        lat: 31.0,
        lon: -5.0,
    },
    Centre {
        name: "north-pole",
        lat: 85.0,
        lon: 0.0,
    },
    Centre {
        name: "south-pole",
        lat: -85.0,
        lon: 0.0,
    },
    Centre {
        name: "antimeridian",
        lat: -17.0,
        lon: 179.99,
    },
    // Where the walk's maps directory holds regions: street level drawn
    // from data, not the radar.
    Centre {
        name: "berlin",
        lat: 52.52,
        lon: 13.405,
    },
    Centre {
        name: "florence",
        lat: 43.7752,
        lon: 11.266,
    },
];

/// The regions every walk home holds (`leviculum-map/fixtures`).
const REGIONS: [&str; 4] = [
    "firenze.lmr",
    "berlin/wide.lmr",
    "berlin/mitte.lmr",
    "frankfurt/roemer.lmr",
];

const HZ: f64 = 60.0;
const NOW: WallTime = WallTime {
    unix_secs: 1_790_000_000,
    offset_secs: 0,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    OversizedBitmap,
    BlankHalf,
    StrayLabel,
    ErrorText,
    SlowFrame,
    HeapKept,
    ViewLost,
}

impl Fault {
    fn of(word: &str) -> Option<Self> {
        Some(match word {
            "oversized-bitmap" => Self::OversizedBitmap,
            "blank-half" => Self::BlankHalf,
            "stray-label" => Self::StrayLabel,
            "error-text" => Self::ErrorText,
            "slow-frame" => Self::SlowFrame,
            "heap-kept" => Self::HeapKept,
            "view-lost" => Self::ViewLost,
            _ => return None,
        })
    }
}

/// The walk's state: where it writes, and Java's side of the last frame.
struct Walk {
    out: PathBuf,
    variant: String,
    rows: std::fs::File,
    fault: Option<Fault>,
    homes: PathBuf,
    failed: usize,
}

/// Java's view of one map screen: whether the last frame turned, the
/// screen it shows, the Bitmap it reuses.
struct Java {
    dir: String,
    size: Size,
    turns: bool,
    screen: Screen,
    bitmap: JavaBitmap,
}

/// One drawn frame: what the library said, its time, its names.
struct Drawn {
    shown: MapShown,
    ms: f64,
    names: Vec<DrawnName>,
}

impl Java {
    fn new(dir: &Path, size: Size) -> Self {
        Self {
            dir: dir.display().to_string(),
            size,
            turns: false,
            screen: Screen::default(),
            bitmap: JavaBitmap::new(0, 0),
        }
    }

    /// The request Java makes: its screen, its margins, `moving`, the
    /// view of the last frame.
    fn ask(&self, moving: bool) -> MapRequest {
        let s = self.size;
        MapRequest {
            dir: self.dir.clone(),
            w: s.w,
            h: s.h,
            density_milli: s.density_milli,
            margin_x: java_margin(s.w, self.turns),
            margin_y: java_margin(s.h, self.turns),
            moving,
            about_x: (s.w / 2) as i32,
            about_y: (s.h / 2) as i32,
            ..MapRequest::default()
        }
    }

    /// `ask` drawn as `MapView` draws it: into a Bitmap of the frame Java
    /// expects, the screen kept or, on a failure, the last scaled.
    fn draw(&mut self, ask: &MapRequest, fault: Option<Fault>) -> Drawn {
        let (bw, bh) = (ask.w + 2 * ask.margin_x, ask.h + 2 * ask.margin_y);
        let (bw, bh) = match fault {
            Some(Fault::OversizedBitmap) => (bw * 3 / 2, bh * 3 / 2),
            _ => (bw, bh),
        };
        if self.bitmap.size != (bw, bh) {
            self.bitmap = JavaBitmap::new(bw, bh);
        } else {
            self.bitmap.clear();
        }
        let t = Instant::now();
        if fault == Some(Fault::SlowFrame) && ask.moving {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut shown = map::render_into(ask, Some(Vec::new()), NOW, &mut self.bitmap);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let asked = ask.zoom_milli.unwrap_or(self.screen.zoom_milli);
        self.screen = match shown.ok {
            true => Screen::landed(
                &self.bitmap.rgba,
                shown.w,
                (shown.margin_x, shown.margin_y),
                (ask.w, ask.h),
                shown.zoom_milli,
            ),
            false => self.screen.failed((ask.w, ask.h), asked),
        };
        if shown.ok {
            self.turns = shown.turns;
        }
        let mut names = map::drawn_names(Path::new(&self.dir)).unwrap_or_default();
        match fault {
            Some(Fault::BlankHalf) => {
                let half = self.screen.rgba.len() / 2;
                for px in self.screen.rgba[half..].chunks_exact_mut(4) {
                    px.copy_from_slice(&SENTINEL);
                }
            }
            Some(Fault::ErrorText) => shown.notice = Some("injected notice".to_string()),
            Some(Fault::StrayLabel) => names.extend(stray(&shown, ask)),
            _ => {}
        }
        Drawn { shown, ms, names }
    }

    /// The view the last frame showed, asked again at rest until its
    /// fades and tiles are done, on a held clock: what a person sees once
    /// the screen is still.
    fn settle(&mut self, mut ask: MapRequest, fault: Option<Fault>) -> Drawn {
        let dir = PathBuf::from(&self.dir);
        let mut clock = 10_000u64;
        map::tune_clock(&dir, Some(clock));
        let mut drawn = self.draw(&ask, fault);
        for _ in 0..40 {
            if !drawn.shown.ok || !drawn.shown.pending {
                break;
            }
            clock += 100;
            map::tune_clock(&dir, Some(clock));
            ask = MapRequest {
                x: Some(drawn.shown.x),
                y: Some(drawn.shown.y),
                zoom_milli: Some(drawn.shown.zoom_milli),
                at_lat_e6: None,
                at_lon_e6: None,
                at_zoom_milli: None,
                margin_x: java_margin(ask.w, self.turns),
                margin_y: java_margin(ask.h, self.turns),
                ..ask
            };
            drawn = self.draw(&ask, fault);
        }
        map::tune_clock(&dir, None);
        drawn
    }
}

/// Four names that break every label rule: across the right edge, on the
/// friends button, over each other, of zoom 20.
fn stray(shown: &MapShown, ask: &MapRequest) -> Vec<DrawnName> {
    let (mx, my) = (shown.margin_x as i32, shown.margin_y as i32);
    let (w, h) = (ask.w as i32, ask.h as i32);
    let d = ask.density_milli as i32 / 1000;
    let at = |x0: i32, y0: i32, x1: i32, y1: i32| Rect {
        x0: x0 + mx,
        y0: y0 + my,
        x1: x1 + mx,
        y1: y1 + my,
    };
    let name = |text: &str, r: Rect| DrawnName {
        text: text.to_string(),
        kind: leviculum_map::labels::LabelKind::City,
        min_zoom: 20,
        at: (0.0, 0.0),
        angle: 0.0,
        boxes: vec![r],
        alpha: 255,
        shown: true,
    };
    vec![
        name("injected edge", at(w - 40, h / 3, w + 40, h / 3 + 20)),
        name(
            "injected button",
            at(w - 60 * d, h - 50 * d, w - 30 * d, h - 30 * d),
        ),
        name("injected pile", at(w / 4, h / 4, w / 4 + 80, h / 4 + 20)),
        name(
            "injected pile too",
            at(w / 4 + 10, h / 4 + 5, w / 4 + 90, h / 4 + 25),
        ),
    ]
}

impl Walk {
    /// A fresh app home: the regions, no autoload.
    fn home(&self, name: &str) -> PathBuf {
        let home = self.homes.join(name);
        let _ = std::fs::remove_dir_all(&home);
        let maps = home.join("maps");
        if let Err(e) = std::fs::create_dir_all(&maps) {
            die(&format!("{}: {e}", maps.display()));
        }
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        for region in REGIONS {
            let to = maps.join(region.replace('/', "-"));
            if let Err(e) = std::fs::copy(fixtures.join(region), &to) {
                die(&format!("{region}: {e}"));
            }
        }
        if let Err(e) = std::fs::write(
            home.join(loquax::config::CONFIG_FILE),
            "map_autoload = off\n",
        ) {
            die(&format!("config: {e}"));
        }
        home
    }

    /// One TSV row a verdict, `at` its kind, sequence, size, centre, zoom
    /// and image.
    fn rows(&mut self, at: [&str; 6], verdicts: &[Verdict]) {
        let [kind, sequence, size, centre, zoom, image] = at;
        let mut text = String::new();
        let clean = |s: &str| s.replace(['\t', '\n'], " ");
        for v in verdicts {
            if !v.pass {
                self.failed += 1;
            }
            let _ = writeln!(
                text,
                "{}\t{kind}\t{sequence}\t{size}\t{centre}\t{zoom}\t{image}\t{}\t{:.3}\t{:.3}\t{}\t{}",
                self.variant,
                v.rule,
                v.value,
                v.limit,
                if v.pass { "pass" } else { "FAIL" },
                clean(&v.note)
            );
        }
        if let Err(e) = self.rows.write_all(text.as_bytes()) {
            die(&format!("rows: {e}"));
        }
    }

    /// The frame rules of `drawn` on `java`'s screen, the thumbnail, and
    /// the whole screen when a rule failed; the image's name.
    fn judge(&mut self, java: &Java, drawn: &Drawn, settled: bool, image: &str) -> Vec<Verdict> {
        let table = match drawn.shown.dark {
            true => MapTheme::Dark,
            false => MapTheme::Light,
        };
        if std::env::var("RENDER_WALK_NAMES").as_deref() == Ok(image) {
            for n in &drawn.names {
                eprintln!(
                    "NAME {:?} alpha={} shown={} boxes={:?} margin={},{}",
                    n.text, n.alpha, n.shown, n.boxes, drawn.shown.margin_x, drawn.shown.margin_y
                );
            }
        }
        let verdicts = frame_rules(&FrameFacts {
            shown: &drawn.shown,
            screen: &java.screen,
            view: (java.size.w, java.size.h),
            density: java.size.density_milli as f32 / 1000.0,
            names: &drawn.names,
            settled,
            space: leviculum_mapfill::frame::rgb888(leviculum_map::outline::space(table)),
        });
        write_png(
            &self.out.join("thumb").join(format!("{image}.png")),
            &java.screen,
            480,
        );
        if verdicts.iter().any(|v| !v.pass) {
            write_png(
                &self.out.join("full").join(format!("{image}.png")),
                &java.screen,
                u32::MAX,
            );
        }
        verdicts
    }

    /// The zoom ladder at `centre` on `size`: from the world at its
    /// smallest to the deepest zoom, each zoom settled and judged.
    fn ladder(&mut self, size: Size, centre: Centre, quick: bool) {
        let home = self.home(&format!("ladder-{}-{}", size.name, centre.name));
        map::hold_rung(&home, Some(0));
        let mut java = Java::new(&home, size);
        let at = project(LatLon::from_degrees(centre.lat, centre.lon));
        let first = MapRequest {
            x: Some(at.x),
            y: Some(at.y),
            zoom_milli: Some(0),
            ..java.ask(false)
        };
        let probe = java.draw(&first, None);
        let (floor, top) = match probe.shown.ok {
            true => (
                probe.shown.min_zoom_milli,
                u32::from(probe.shown.max_zoom) * MILLI,
            ),
            false => (2 * MILLI, 16 * MILLI),
        };
        let mut zooms = vec![floor];
        let mut z = floor.div_ceil(MILLI) * MILLI;
        if z == floor {
            z += MILLI;
        }
        while z <= top {
            zooms.push(z);
            z += MILLI;
        }
        if quick {
            zooms.retain(|&z| z == floor || [4, 6, 8, 10, 12, 14, 16].contains(&(z / MILLI)));
        }
        for zoom in zooms {
            let ask = MapRequest {
                x: Some(at.x),
                y: Some(at.y),
                zoom_milli: Some(zoom),
                ..java.ask(false)
            };
            let drawn = java.settle(ask, self.fault);
            let image = format!(
                "{}-ladder-{}-{}-z{:05}",
                self.variant, size.name, centre.name, zoom
            );
            let verdicts = self.judge(&java, &drawn, true, &image);
            let words = format!("{:.3}", f64::from(zoom) / f64::from(MILLI));
            self.rows(
                ["frame", "ladder", size.name, centre.name, &words, &image],
                &verdicts,
            );
        }
        map::tune_tiles(&home, None, true);
    }

    /// A gesture: `steps` moving frames from the settled view `start`,
    /// each asked by `step` from the last frame, then a frame at rest.
    /// Every frame judged; the frame times and the heap judged at the end.
    fn gesture(
        &mut self,
        name: &str,
        size: Size,
        centre: Centre,
        zoom_milli: u32,
        steps: usize,
        step: &dyn Fn(usize, &MapShown, MapRequest) -> Option<MapRequest>,
    ) {
        let home = self.home(&format!("gesture-{name}-{}", size.name));
        let mut java = Java::new(&home, size);
        let at = project(LatLon::from_degrees(centre.lat, centre.lon));
        let start = MapRequest {
            x: Some(at.x),
            y: Some(at.y),
            zoom_milli: Some(zoom_milli),
            ..java.ask(false)
        };
        let mut last = java.settle(start, self.fault).shown;
        reset_peak();
        let heap_start = live();
        let mut held: Vec<u8> = Vec::new();
        if self.fault == Some(Fault::HeapKept) {
            held = vec![1u8; 384 << 20];
        }
        let mut played = Played {
            hz: HZ,
            heap_start,
            ..Played::default()
        };
        for i in 0..steps {
            let base = MapRequest {
                x: Some(last.x),
                y: Some(last.y),
                zoom_milli: Some(last.zoom_milli),
                ..java.ask(true)
            };
            let Some(ask) = step(i, &last, base) else {
                break;
            };
            let drawn = java.draw(&ask, self.fault);
            played.frame_ms.push(drawn.ms);
            let image = format!("{}-{name}-{}-f{i:03}", self.variant, size.name);
            let verdicts = self.judge(&java, &drawn, false, &image);
            let zoom = format!(
                "{:.3}",
                f64::from(drawn.shown.zoom_milli) / f64::from(MILLI)
            );
            self.rows(
                ["frame", name, size.name, centre.name, &zoom, &image],
                &verdicts,
            );
            if drawn.shown.ok {
                last = drawn.shown;
            }
        }
        let rest = MapRequest {
            x: Some(last.x),
            y: Some(last.y),
            zoom_milli: Some(last.zoom_milli),
            ..java.ask(false)
        };
        let drawn = java.settle(rest, self.fault);
        let image = format!("{}-{name}-{}-rest", self.variant, size.name);
        let verdicts = self.judge(&java, &drawn, true, &image);
        let zoom = format!(
            "{:.3}",
            f64::from(drawn.shown.zoom_milli) / f64::from(MILLI)
        );
        self.rows(
            ["frame", name, size.name, centre.name, &zoom, &image],
            &verdicts,
        );
        played.heap_peak = PEAK.load(Ordering::Relaxed);
        played.heap_end = live();
        std::hint::black_box(&held);
        drop(held);
        let verdicts = sequence_rules(&played);
        self.rows(
            ["sequence", name, size.name, centre.name, "", &image],
            &verdicts,
        );
        map::tune_tiles(&home, None, true);
    }

    /// The phone turned to landscape and back at rest: the view and its
    /// zoom where they were (the library's half of walk finding 10).
    fn rotation(&mut self, size: Size, centre: Centre) {
        let home = self.home(&format!("gesture-rotate-{}", size.name));
        let mut java = Java::new(&home, size);
        let at = project(LatLon::from_degrees(centre.lat, centre.lon));
        let start = MapRequest {
            x: Some(at.x),
            y: Some(at.y),
            zoom_milli: Some(9 * MILLI),
            ..java.ask(false)
        };
        reset_peak();
        let heap_start = live();
        let mut played = Played {
            hz: HZ,
            heap_start,
            ..Played::default()
        };
        let before = java.settle(start, self.fault).shown;
        let turned = Size {
            name: "turned",
            w: size.h,
            h: size.w,
            ..size
        };
        let mut shown = before.clone();
        for (i, s) in [turned, size].into_iter().enumerate() {
            java.size = s;
            let ask = MapRequest {
                x: Some(shown.x),
                y: Some(shown.y),
                zoom_milli: Some(shown.zoom_milli),
                ..java.ask(false)
            };
            let drawn = java.settle(ask, self.fault);
            played.frame_ms.push(drawn.ms);
            let image = format!("{}-rotate-{}-{i}", self.variant, size.name);
            let verdicts = self.judge(&java, &drawn, true, &image);
            let zoom = format!(
                "{:.3}",
                f64::from(drawn.shown.zoom_milli) / f64::from(MILLI)
            );
            self.rows(
                ["frame", "rotate", s.name, centre.name, &zoom, &image],
                &verdicts,
            );
            if drawn.shown.ok {
                shown = drawn.shown;
            }
        }
        if self.fault == Some(Fault::ViewLost) {
            shown.x = shown.x.wrapping_add(1 << 20);
        }
        // How far the centre moved, in screen pixels at the zoom before.
        let unit = (32.0 - f64::from(before.zoom_milli) / f64::from(MILLI) - 8.0).exp2();
        let dx = (f64::from(shown.x) - f64::from(before.x)) / unit;
        let dy = (f64::from(shown.y) - f64::from(before.y)) / unit;
        played.moved_px = Some((dx * dx + dy * dy).sqrt());
        played.zoom_moved_milli = Some(shown.zoom_milli.abs_diff(before.zoom_milli));
        played.heap_peak = PEAK.load(Ordering::Relaxed);
        played.heap_end = live();
        let verdicts = sequence_rules(&played);
        let image = format!("{}-rotate-{}-1", self.variant, size.name);
        self.rows(
            ["sequence", "rotate", size.name, centre.name, "", &image],
            &verdicts,
        );
        map::tune_tiles(&home, None, true);
    }

    /// The five gestures on `size`.
    fn gestures(&mut self, size: Size) {
        let [germany, _, _, _, _, berlin, _] = CENTRES;
        // Across tile seams at z13 over Berlin's region: 40 px a frame
        // east, 24 frames, three seams.
        self.gesture("pan-seam", size, berlin, 13 * MILLI, 24, &|_, _, ask| {
            Some(MapRequest { pan_x: 40, ..ask })
        });
        // Pinched out from z12 to the world at its smallest, a quarter
        // step a frame, then back in to z12.
        self.gesture("pinch-out-in", size, germany, 12 * MILLI, 120, &{
            let out = std::cell::Cell::new(true);
            move |_, last: &MapShown, ask| {
                if out.get() && last.zoom_milli <= last.min_zoom_milli {
                    out.set(false);
                }
                if !out.get() && last.zoom_milli >= 12 * MILLI {
                    return None;
                }
                let by = if out.get() { -250 } else { 250 };
                Some(MapRequest {
                    zoom_by_milli: by,
                    ..ask
                })
            }
        });
        // The world dragged toward the north pole, then around: 40 px a
        // frame, 30 frames each way. The globe turns, Equal Earth pans.
        self.gesture(
            "drag-pole-around",
            size,
            germany,
            3 * MILLI,
            60,
            &|i, _, ask| {
                Some(match i < 30 {
                    true => MapRequest { pan_y: -40, ..ask },
                    false => MapRequest { pan_x: 40, ..ask },
                })
            },
        );
        // A jump from the street to Europe, as a tap on a position or the
        // search asks it: the street's names must not stay.
        let jump = LatLon::from_degrees(50.0, 10.0);
        self.gesture(
            "jump-street-to-z5",
            size,
            berlin,
            16 * MILLI,
            1,
            &move |_, _, ask| {
                Some(MapRequest {
                    at_lat_e6: Some(jump.lat_e7 / 10),
                    at_lon_e6: Some(jump.lon_e7 / 10),
                    at_zoom_milli: Some(5 * MILLI),
                    moving: false,
                    ..ask
                })
            },
        );
        self.rotation(size, germany);
    }
}

/// `screen` as a PNG at most `max_h` pixels high (a whole-pixel step
/// down), the undrawn pixels magenta.
fn write_png(path: &Path, screen: &Screen, max_h: u32) {
    if screen.w == 0 || screen.h == 0 {
        return;
    }
    let step = screen.h.div_ceil(max_h).max(1);
    let (w, h) = (screen.w / step, screen.h / step);
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * step) as usize * screen.w as usize + (x * step) as usize) * 4;
            match screen.rgba.get(i..i + 4) {
                Some(px) if px == SENTINEL => rgb.extend_from_slice(&[255, 0, 255]),
                Some(px) => rgb.extend_from_slice(&px[..3]),
                None => rgb.extend_from_slice(&[255, 0, 255]),
            }
        }
    }
    let written = (|| -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgb).map_err(|e| e.to_string())
    })();
    if let Err(e) = written {
        die(&format!("{}: {e}", path.display()));
    }
}

fn die(why: &str) -> ! {
    eprintln!("render_walk: {why}");
    std::process::exit(2);
}

fn main() {
    let mut out = None;
    let mut variant = None;
    let mut quick = false;
    let mut selftest = false;
    let mut fault = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--variant" => variant = args.next(),
            "--quick" => quick = true,
            "--selftest" => selftest = true,
            "--inject" => {
                let word = args.next().unwrap_or_default();
                fault = Some(Fault::of(&word).unwrap_or_else(|| die(&format!("no fault {word}"))));
            }
            other => die(&format!("unknown argument {other}")),
        }
    }
    let (Some(out), Some(variant)) = (out, variant) else {
        die("usage: render_walk --out DIR --variant NAME [--quick | --selftest] [--inject FAULT]");
    };
    let homes = out.join("homes").join(&variant);
    if let Err(e) = std::fs::create_dir_all(&homes) {
        die(&format!("{}: {e}", homes.display()));
    }
    let rows = std::fs::File::create(out.join(format!("rows-{variant}.tsv")))
        .unwrap_or_else(|e| die(&format!("rows: {e}")));
    let mut walk = Walk {
        out: out.clone(),
        variant,
        rows,
        fault,
        homes,
        failed: 0,
    };
    let started = Instant::now();
    let [phone, _, _, window, _] = SIZES;
    let [germany, _, north, _, _, berlin, _] = CENTRES;
    if selftest {
        // The positive controls' small walk: one screen, the quick
        // ladder over Berlin's data, and every gesture.
        walk.ladder(phone, berlin, true);
        walk.gestures(phone);
    } else if quick {
        for size in [phone, window] {
            for centre in [germany, north, berlin] {
                walk.ladder(size, centre, true);
            }
        }
        walk.gestures(phone);
    } else {
        for size in SIZES {
            for centre in CENTRES {
                walk.ladder(size, centre, false);
            }
            walk.gestures(size);
        }
    }
    let _ = std::fs::remove_dir_all(&walk.homes);
    println!(
        "render_walk: {} {} failed verdicts in {:.0} s",
        walk.variant,
        walk.failed,
        started.elapsed().as_secs_f64()
    );
}
