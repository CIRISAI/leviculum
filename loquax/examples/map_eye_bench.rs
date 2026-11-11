//! The map's frame budget measured (1422, the eye candy study of
//! 2026-11-04, section 4): one `MAP_BENCH` line per scene, size and zoom,
//! through the APIs every frontend draws with. It folds the scattered
//! examples before it (`map_bench`, `window_fps`, `names_cost`, the
//! phone's `map_fps`, the M9's host `map_measure`) into one line format a
//! table and a regression check read. A measuring tool for the reviewer
//! and the commit messages, never a test gate: the gates count edges,
//! spans and pixels (`leviculum-map/tests/golden.rs`), never clocks.
//!
//! ```sh
//! cargo run --release -p loquax --features screenmap --example map_eye_bench -- MAPS [--quick] [--no-ladder] [--label L] [--against BIN | --baseline FILE] [--write FILE]
//! ```
//!
//! `MAPS` holds the Frankfurt region (LMR3 with names and buildings, the
//! study's file, sha256 `5022565b...a2b7c7a`) and nothing else; `just
//! map-bench` sets that up and runs this on the host and under the weak
//! CPU emulation. Each scene runs in a child process of its own, so its
//! `heap_peak_kib` is the child's own `VmHWM` (loquax forbids `unsafe`,
//! so no counting allocator), the peak resident set the scene reached.
//!
//! The columns, all milliseconds, the median of the repetitions, on one
//! worker thread unless they say otherwise:
//!
//! - `ground_ms`: the whole view drawn by [`Painter::draw`] on one
//!   thread, no tiles, no names: the ground pass.
//! - `compose_ms`: a warm frame at rest without names, the tiles composed.
//! - `frame_ms`: a warm frame at rest with its names and the pins.
//! - `labels_ms`: the same frame with the names' glyphs and halos
//!   forgotten ([`Frames::forget_names`]), less `frame_ms`.
//! - `tracks_ms`: a frame with a track of 400 segments, less `frame_ms`.
//! - `cold_ms`: a frame at rest from an empty cache, one worker;
//!   `cold_nw_ms` the same on every worker the cache takes (not held).
//! - `pan_ms`: a warm frame that moves 40 pixels a frame.
//! - `zoom_ms`: a warm frame that zooms by a tenth of a step a frame.
//!
//! With `--against BIN`, the bench of an earlier commit, each scene is
//! measured by that binary and this one in turn, five rounds, and each
//! column's median of its five compared (the A/B the study asks: the
//! host's other work moved a phone frame's compose from 3.2 to 4.8 ms
//! between two runs of one binary, 1422). With `--baseline FILE` instead,
//! the lines of `FILE`, a committed run, are the reference, and a scene
//! that reads slower is measured twice more, its best kept. Either way
//! every column but `labels_ms` and `tracks_ms` (differences of two
//! medians) is held: more than 5 % and more than 0.1 ms slower is a
//! regression.
//! The process then exits 1. `--write FILE` writes the lines measured.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use leviculum_map::draw::Ink;
use leviculum_map::ladder::{Layers, Pass, BUDGET_US, RUNGS};
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::viewport::{ExactView, Viewport, MILLI};
use leviculum_mapfill::style::Style;
use loquax::mapframe::{Ask, Frames, Shot};
use loquax::picture::{Maps, Painter};
use loquax::ui::map::MapPin;
use loquax::ui::tracks::{Age, Track};

/// One scene: a place, a frame's size, its density and zoom.
#[derive(Clone, Copy)]
struct Scene {
    name: &'static str,
    w: u32,
    h: u32,
    /// Screen pixels a logical one: the phone's 2.75.
    scale: f32,
    zoom: u8,
    /// Pixels of margin each side, a share of the screen: the window's
    /// eighth, the phone's quarter.
    margin: u32,
    style: Style,
}

const SCENES: &[Scene] = &[
    Scene::small(13),
    Scene::small(15),
    Scene::small(17),
    Scene::phone(13),
    Scene::phone(15),
    Scene::phone(17),
    Scene::m9(13),
    Scene::m9(15),
    Scene::m9(17),
];

impl Scene {
    const fn small(zoom: u8) -> Self {
        Self {
            name: "ffm",
            w: 480,
            h: 320,
            scale: 1.0,
            zoom,
            margin: 8,
            style: Style::SCREEN,
        }
    }

    const fn phone(zoom: u8) -> Self {
        Self {
            name: "ffm-phone",
            w: 1080,
            h: 2400,
            scale: 2.75,
            zoom,
            margin: 4,
            style: Style::SCREEN,
        }
    }

    /// The M9's screen, host rendered in its own finish.
    const fn m9(zoom: u8) -> Self {
        Self {
            name: "ffm-m9",
            w: 320,
            h: 240,
            scale: 1.0,
            zoom,
            margin: 8,
            style: Style::M9,
        }
    }

    fn key(&self) -> String {
        format!("{} {}x{} {}", self.name, self.w, self.h, self.zoom)
    }
}

/// The columns held to the baseline.
const HELD: &[&str] = &[
    "ground_ms",
    "compose_ms",
    "frame_ms",
    "cold_ms",
    "pan_ms",
    "zoom_ms",
];

/// Slower than the baseline by more than this share is a regression.
const SLOWER: f64 = 0.05;
/// And by more than this many milliseconds: below it a column moves with
/// the host's other work from run to run (a pan of 0.32 ms read 0.35 in
/// the next run of the same binary, 1422).
const SLACK_MS: f64 = 0.1;
/// Rounds of an A/B run ([`main`]'s `--against`).
const AB_ROUNDS: usize = 5;

fn median(mut ms: Vec<f64>) -> f64 {
    ms.sort_by(f64::total_cmp);
    ms.get(ms.len() / 2).copied().unwrap_or(0.0)
}

fn timed(f: impl FnOnce()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1e3
}

fn centre() -> World {
    project(LatLon::from_degrees(50.1109, 8.6821))
}

/// A few pins about the centre, one of them ours.
fn pins(at: World) -> Vec<MapPin> {
    (0..6u8)
        .map(|i| MapPin {
            peer: [i; 16],
            at: World {
                x: at.x.wrapping_add(u32::from(i) * 3000),
                y: at.y.wrapping_add(u32::from(i) * 2000),
            },
            label: format!("peer {i}"),
            age: None,
            stale: false,
            nearby: false,
            own: i == 0,
            fix_at: None,
            battery: None,
        })
        .collect()
}

/// One track of 400 segments winding about the centre.
fn tracks(at: World) -> Vec<Track> {
    let point = |i: u32| {
        let t = f64::from(i) / 40.0;
        World {
            x: at
                .x
                .wrapping_add((t.cos() * 20_000.0 + f64::from(i) * 40.0) as i32 as u32),
            y: at.y.wrapping_add((t.sin() * 15_000.0) as i32 as u32),
        }
    };
    let segments = (0..400u32)
        .map(|i| loquax::ui::tracks::Segment {
            from: point(i),
            to: point(i + 1),
            age: match i {
                0..=99 => Age::Faint,
                100..=199 => Age::Half,
                200..=299 => Age::Strong,
                _ => Age::Fresh,
            },
            late: i % 37 == 0,
        })
        .collect();
    vec![Track {
        peer: [1; 16],
        hue: Some(3),
        segments,
        dots: Vec::new(),
        dwells: Vec::new(),
        places: Vec::new(),
        wedge: None,
        head: Some(point(400)),
    }]
}

/// The peak resident set of this process, from `/proc/self/status`.
fn vm_hwm_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}

struct Reps {
    /// Repetitions of a warm measurement, and of a cold one.
    warm: usize,
    cold: usize,
    /// The frame budget's traces after the line (`--no-ladder` drops
    /// them).
    ladder: bool,
}

/// Measure `scene` in this process and print its line.
fn run_scene(maps: &Path, scene: Scene, reps: &Reps) {
    let at = centre();
    let (w, h) = (scene.w, scene.h);
    let mut painter = Painter::new(Maps::new(Some(maps.to_path_buf())));
    let percent = (scene.scale * 100.0).round() as u32;
    let view = Viewport::new(at, scene.zoom, w, h);
    let _ = painter.draw(&view, Ink::Colour, scene.style, percent);
    let ground = median(
        (0..reps.warm)
            .map(|_| {
                timed(|| {
                    let drawn = painter.draw(&view, Ink::Colour, scene.style, percent);
                    std::hint::black_box(drawn.fb.len());
                })
            })
            .collect(),
    );
    drop(painter);

    let pins = pins(at);
    let tracks = tracks(at);
    let mut frames = Frames::new(Maps::new(Some(maps.to_path_buf())));
    // One worker for every held column: the study's A/B is on one
    // thread, the host's other work moves a frame on eight far more.
    let workers = frames.workers();
    frames.set_workers(1);
    let margin = (w / scene.margin, h / scene.margin);
    let base = ExactView::new(at, u32::from(scene.zoom) * MILLI, w, h);
    let frame = |frames: &mut Frames, view: ExactView, moving: bool, tracks: &[Track]| {
        let shot = frames.frame(&Ask {
            view,
            margin,
            moving,
            style: scene.style,
            scale: scene.scale,
            pins: &pins,
            selected: None,
            zooms: (3, 18),
            draw_pins: true,
            tracks,
            radar: None,
            measure: None,
            now_ms: None,
            scaled_while_moving: true,
            time_zones: None,
        });
        std::hint::black_box(shot.rgba.len());
        frames.recycle(shot.rgba);
    };
    let cold = median(
        (0..reps.cold)
            .map(|_| {
                frames.clear_tiles();
                frames.forget_names();
                timed(|| frame(&mut frames, base, false, &[]))
            })
            .collect(),
    );
    frames.set_workers(workers);
    let cold_nw = median(
        (0..reps.cold)
            .map(|_| {
                frames.clear_tiles();
                frames.forget_names();
                timed(|| frame(&mut frames, base, false, &[]))
            })
            .collect(),
    );
    frames.set_workers(1);
    frame(&mut frames, base, false, &[]);
    let warm = |frames: &mut Frames, tracks: &[Track]| {
        median(
            (0..reps.warm)
                .map(|_| timed(|| frame(frames, base, false, tracks)))
                .collect(),
        )
    };
    let frame_ms = warm(&mut frames, &[]);
    let with_tracks = warm(&mut frames, &tracks);
    let replaced = median(
        (0..reps.warm)
            .map(|_| {
                // Every frame at rest without a clock places its names;
                // forgotten, their glyphs and halos are set anew too.
                frames.forget_names();
                timed(|| frame(&mut frames, base, false, &[]))
            })
            .collect(),
    );
    frames.set_names_off(true);
    frame(&mut frames, base, false, &[]);
    let compose = warm(&mut frames, &[]);
    frames.set_names_off(false);
    frame(&mut frames, base, false, &[]);
    let pan = median(
        (1..=reps.warm as i32)
            .map(|i| {
                let mut v = base;
                v.pan(40 * i, 0);
                timed(|| frame(&mut frames, v, true, &[]))
            })
            .collect(),
    );
    frame(&mut frames, base, false, &[]);
    let zoom = median(
        (1..=reps.warm as u32)
            .map(|i| {
                let mut v = base;
                let step = u32::from(scene.zoom) * MILLI + (i % 9 + 1) * 100;
                v.zoom_about(step, f64::from(w) / 2.0, f64::from(h) / 2.0);
                timed(|| frame(&mut frames, v, true, &[]))
            })
            .collect(),
    );
    println!(
        "MAP_BENCH scene={} size={w}x{h} zoom={} ground_ms={ground:.2} labels_ms={:.2} tracks_ms={:.2} compose_ms={compose:.2} frame_ms={frame_ms:.2} cold_ms={cold:.2} cold_nw_ms={cold_nw:.2} pan_ms={pan:.2} zoom_ms={zoom:.2} heap_peak_kib={}",
        scene.name,
        scene.zoom,
        (replaced - frame_ms).max(0.0),
        (with_tracks - frame_ms).max(0.0),
        vm_hwm_kib(),
    );
    if reps.ladder {
        let scene_key = format!("scene={} size={w}x{h} zoom={}", scene.name, scene.zoom);
        let ask = |view: ExactView, moving: bool, now_ms: u64| Ask {
            view,
            margin,
            moving,
            style: scene.style,
            scale: scene.scale,
            pins: &pins,
            selected: None,
            zooms: (3, 18),
            draw_pins: true,
            tracks: &tracks,
            radar: None,
            measure: None,
            now_ms: Some(now_ms),
            scaled_while_moving: true,
            time_zones: None,
        };
        ladder(&mut frames, &scene_key, base, &ask);
    }
}

/// The names of the passes in `layers`, or `-`.
fn names(layers: Layers) -> String {
    let n: Vec<&str> = Pass::ALL
        .iter()
        .filter(|p| layers.has(**p))
        .map(|p| p.name())
        .collect();
    match n.is_empty() {
        true => "-".to_string(),
        false => n.join(","),
    }
}

/// The passes of the last frame with their microseconds.
fn pass_us(frames: &Frames) -> String {
    let t: Vec<String> = frames
        .trace()
        .iter()
        .map(|(p, us)| format!("{}:{us}", p.name()))
        .collect();
    t.join(",")
}

/// One frame of `ask`, timed, its buffer given back.
fn shot(frames: &mut Frames, ask: &Ask<'_>) -> (f64, Shot) {
    let t = Instant::now();
    let shot = frames.frame(ask);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    (ms, shot)
}

/// The frame budget's planner at work (1422, study section 4 "je
/// Stufe"): each rung held, a moving frame with its tiles owed and the
/// frame at rest that replaces it (`MAP_LADDER rung=`); then the ladder
/// free: a scripted pan of 60 frames (`MAP_LADDER frame=`), the view at
/// rest growing back pass by pass (`MAP_REFINE step=`) to the full
/// picture (`full_at_ms=`); then the same rest under a memory too small
/// for the names' sprites, which skips them and still ends full.
fn ladder<'a>(
    frames: &mut Frames,
    key: &str,
    base: ExactView,
    ask: &dyn Fn(ExactView, bool, u64) -> Ask<'a>,
) {
    let mut clock = 0u64;
    let mut tick = |ms: f64| {
        clock += (ms.ceil() as u64).max(1);
        clock
    };
    for rung in 0..RUNGS {
        frames.hold_rung(Some(rung));
        let (_, s) = shot(frames, &ask(base, false, tick(0.0)));
        frames.recycle(s.rgba);
        // The tiles owed: only a moving frame that must draw tiles feels
        // the rung in the tile path.
        frames.clear_tiles();
        let mut v = base;
        v.pan(40, 0);
        let (motion, s) = shot(frames, &ask(v, true, tick(0.0)));
        let motion_passes = pass_us(frames);
        frames.recycle(s.rgba);
        let (rest, s) = shot(frames, &ask(v, false, tick(motion)));
        frames.recycle(s.rgba);
        println!(
            "MAP_LADDER {key} rung={rung} motion_ms={motion:.2} rest_ms={rest:.2} motion_pass_us={motion_passes} rest_pass_us={}",
            pass_us(frames)
        );
    }
    frames.hold_rung(None);
    let rest_at = |frames: &mut Frames, label: &str, clock: &mut dyn FnMut(f64) -> u64| {
        let mut full_at = 0.0;
        for step in 0..32 {
            let (ms, s) = shot(frames, &ask(base, false, clock(0.0)));
            let _ = clock(ms);
            full_at += ms;
            let done = s.missing == 0;
            println!(
                "MAP_REFINE {key} run={label} step={step} pass={} ms={ms:.2} free_kib={} skipped={} rung={}",
                names(s.layers),
                frames.free_bytes() / 1024,
                names(frames.rest_skipped()),
                s.rung,
            );
            frames.recycle(s.rgba);
            if done {
                println!(
                    "MAP_REFINE {key} run={label} full_at_ms={full_at:.2} steps={} budget_ms={:.1}",
                    step + 1,
                    f64::from(BUDGET_US) / 1e3
                );
                return;
            }
        }
        println!("MAP_REFINE {key} run={label} full_at_ms=never");
    };
    // The gesture: 60 frames, 8 pixels each, with the clock running.
    let (mut over, mut ups) = (0usize, 0usize);
    let mut last = frames.ladder().rung();
    for i in 0..60 {
        let mut v = base;
        v.pan(8 * (i + 1), 4 * (i + 1));
        let (ms, s) = shot(frames, &ask(v, true, tick(0.0)));
        tick(ms);
        if ms > f64::from(BUDGET_US) / 1e3 {
            over += 1;
        }
        if s.rung < last {
            ups += 1;
        }
        last = s.rung;
        println!(
            "MAP_LADDER {key} frame={i} ms={ms:.2} ema_ms={:.2} rung={} pass_us={}",
            f64::from(frames.ladder().motion_us().unwrap_or(0)) / 1e3,
            frames.ladder().rung(),
            pass_us(frames)
        );
        frames.recycle(s.rgba);
    }
    println!(
        "MAP_LADDER {key} gesture_end rung={last} frames_over_budget={over} ups_in_gesture={ups}"
    );
    rest_at(frames, "free", &mut tick);
    // Low memory: the tiles held plus a few KiB, so the names' sprites
    // do not fit; the picture still ends full, the names skipped.
    let mut v = base;
    v.pan(4, 0);
    let (_, s) = shot(frames, &ask(v, true, tick(0.0)));
    frames.recycle(s.rgba);
    frames.set_heap_limit(Some(frames.tile_memory().1 + 4096));
    rest_at(frames, "low_memory", &mut tick);
    frames.set_heap_limit(None);
}

/// A `MAP_BENCH` line's scene key and its columns.
fn parse(line: &str) -> Option<(String, BTreeMap<String, f64>)> {
    let rest = line.strip_prefix("MAP_BENCH ")?;
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for kv in rest.split_whitespace() {
        let (k, v) = kv.split_once('=')?;
        fields.insert(k.to_string(), v.to_string());
    }
    let key = format!(
        "{} {} {}",
        fields.get("scene")?,
        fields.get("size")?,
        fields.get("zoom")?
    );
    let cols = fields
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.parse::<f64>().ok()?)))
        .collect();
    Some((key, cols))
}

/// `cols` of the scene `key` as a `MAP_BENCH` line again.
fn line(key: &str, cols: &BTreeMap<String, f64>) -> String {
    let mut parts = key.split(' ');
    let mut l = format!(
        "MAP_BENCH scene={} size={} zoom={}",
        parts.next().unwrap_or("-"),
        parts.next().unwrap_or("-"),
        parts.next().unwrap_or("-")
    );
    for (k, v) in cols.iter().filter(|(k, _)| k.as_str() != "zoom") {
        match k.as_str() {
            "heap_peak_kib" => l.push_str(&format!(" {k}={v:.0}")),
            _ => l.push_str(&format!(" {k}={v:.2}")),
        }
    }
    l
}

/// The child's `MAP_BENCH` line for `scene`, and any other lines it
/// printed.
fn child(bin: &Path, maps: &Path, scene: &Scene, quick: bool) -> Vec<String> {
    let mut cmd = Command::new(bin);
    cmd.arg(maps).arg("--scene").arg(scene.key());
    if quick {
        cmd.arg("--quick");
    }
    match cmd.output() {
        Ok(out) => {
            if !out.status.success() {
                eprintln!(
                    "scene {}: {}",
                    scene.key(),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        }
        Err(e) => {
            eprintln!("scene {}: {e}", scene.key());
            Vec::new()
        }
    }
}

/// A table of the `MAP_BENCH` lines among `lines`, one row a scene.
fn table(label: &str, lines: &[String]) {
    println!();
    println!(
        "| {label} | ground | labels | tracks | compose | frame | cold | cold nw | pan | zoom | peak KiB |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for (key, cols) in lines.iter().filter_map(|l| parse(l)) {
        let c = |k: &str| cols.get(k).map_or("-".to_string(), |v| format!("{v:.2}"));
        println!(
            "| {key} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c("ground_ms"),
            c("labels_ms"),
            c("tracks_ms"),
            c("compose_ms"),
            c("frame_ms"),
            c("cold_ms"),
            c("cold_nw_ms"),
            c("pan_ms"),
            c("zoom_ms"),
            cols.get("heap_peak_kib")
                .map_or("-".to_string(), |v| format!("{v:.0}")),
        );
    }
}

/// Each column's median over `runs`.
fn medians(runs: &[BTreeMap<String, f64>]) -> BTreeMap<String, f64> {
    let mut cols: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for run in runs {
        for (k, v) in run {
            cols.entry(k.clone()).or_default().push(*v);
        }
    }
    cols.into_iter().map(|(k, v)| (k, median(v))).collect()
}

/// The held columns of `now` more than [`SLOWER`] above `then`'s.
fn slower(now: &BTreeMap<String, f64>, then: &BTreeMap<String, f64>) -> Vec<String> {
    HELD.iter()
        .filter_map(|&c| {
            let (n, t) = (now.get(c)?, then.get(c)?);
            (*n > t * (1.0 + SLOWER) && n - t > SLACK_MS).then(|| format!("{c} {t:.2} -> {n:.2}"))
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(maps) = args.first().map(PathBuf::from) else {
        eprintln!(
            "usage: map_eye_bench MAPS [--quick] [--label L] [--against BIN | --baseline FILE] [--write FILE]"
        );
        std::process::exit(2);
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let quick = flag("--quick");
    let ladder = !flag("--no-ladder");
    let reps = match quick {
        true => Reps {
            warm: 3,
            cold: 1,
            ladder,
        },
        false => Reps {
            warm: 9,
            cold: 5,
            ladder,
        },
    };
    if let Some(key) = value("--scene") {
        let Some(scene) = SCENES.iter().find(|s| s.key() == key) else {
            eprintln!("no scene {key}");
            std::process::exit(2);
        };
        run_scene(&maps, *scene, &reps);
        return;
    }
    let label = value("--label").unwrap_or_else(|| "host".to_string());
    let Ok(me) = std::env::current_exe() else {
        eprintln!("no path to this binary");
        std::process::exit(2);
    };
    let against = value("--against").map(PathBuf::from);
    let baseline: BTreeMap<String, BTreeMap<String, f64>> = value("--baseline")
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.lines().filter_map(parse).collect())
        .unwrap_or_default();
    let measured = |out: &[String]| out.iter().find_map(|l| parse(l));
    let mut lines = Vec::new();
    let mut base_lines = Vec::new();
    let mut regressions = Vec::new();
    for scene in SCENES {
        let (out, cols, then) = match &against {
            // A/B in the same run: the other binary and this one in turn,
            // five rounds, each column's median of its five.
            Some(other) => {
                let (mut a, mut b) = (Vec::new(), Vec::new());
                let mut out = Vec::new();
                for _ in 0..AB_ROUNDS {
                    a.extend(measured(&child(other, &maps, scene, quick)).map(|(_, c)| c));
                    out = child(&me, &maps, scene, quick);
                    b.extend(measured(&out).map(|(_, c)| c));
                }
                let then = (!a.is_empty()).then(|| medians(&a));
                if let Some(then) = &then {
                    base_lines.push(line(&scene.key(), then));
                }
                (out, (!b.is_empty()).then(|| medians(&b)), then)
            }
            None => {
                let out = child(&me, &maps, scene, quick);
                let mut cols = measured(&out).map(|(_, c)| c);
                let then = baseline.get(&scene.key()).cloned();
                // Against the file, measured twice more before a slower
                // column counts: the best of three medians.
                if let (Some(cols), Some(then)) = (&mut cols, &then) {
                    for _ in 0..2 {
                        if slower(cols, then).is_empty() {
                            break;
                        }
                        let again = child(&me, &maps, scene, quick);
                        for (k, v) in measured(&again).map(|(_, c)| c).unwrap_or_default() {
                            let best = cols.get(&k).map_or(v, |&c| c.min(v));
                            cols.insert(k, best);
                        }
                    }
                }
                (out, cols, then)
            }
        };
        let mut out: Vec<String> = out
            .into_iter()
            .filter(|l| !l.starts_with("MAP_BENCH "))
            .collect();
        if let Some(cols) = &cols {
            out.push(line(&scene.key(), cols));
            if let Some(then) = &then {
                for s in slower(cols, then) {
                    regressions.push(format!("{}: {s}", scene.key()));
                }
            }
        }
        for l in &out {
            println!("{l}");
        }
        lines.extend(out);
    }
    for l in &base_lines {
        println!("{}", l.replacen("MAP_BENCH ", "MAP_BENCH_BASE ", 1));
    }
    if !base_lines.is_empty() {
        table(&format!("{label} base"), &base_lines);
    }
    table(&label, &lines);
    if let Some(file) = value("--write") {
        let bench: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("MAP_BENCH "))
            .collect();
        let text: String = bench.iter().map(|l| format!("{l}\n")).collect();
        if let Err(e) = std::fs::write(&file, text) {
            eprintln!("{file}: {e}");
        }
    }
    if !regressions.is_empty() {
        println!();
        for r in &regressions {
            println!("MAP_BENCH_SLOWER {r}");
        }
        std::process::exit(1);
    }
}
