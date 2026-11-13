//! The window's drag, wheel and scroll measured (1431): a bench that
//! replays a drag the way iced's loop runs it, events in, the worker's
//! frames back, and the screen rasterised by iced's own software renderer
//! whenever something changed; and the tests of the trace that says what
//! ran per event.

use super::*;
use std::sync::mpsc;

/// What one replayed gesture cost.
#[derive(Debug, Default)]
pub(super) struct Bench {
    pub events: usize,
    /// Frames the screen was rasterised.
    pub frames: usize,
    /// Frames the worker drew.
    pub drawn: usize,
    pub seconds: f64,
    /// Per screen frame: the update of its events, the view and layout,
    /// the rasterising.
    pub update_ms: Vec<f64>,
    pub layout_ms: Vec<f64>,
    pub raster_ms: Vec<f64>,
    /// Per screen frame that showed a new worker frame for the first time.
    pub fresh_raster_ms: Vec<f64>,
    pub worker_ms: Vec<f64>,
}

fn stats(v: &[f64]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    let p95 = s[(s.len() * 95 / 100).min(s.len() - 1)];
    format!(
        "mean {mean:.2} p95 {p95:.2} max {:.2} (n {})",
        s[s.len() - 1],
        s.len()
    )
}

impl Bench {
    pub fn say(&self, label: &str) {
        println!(
            "DRAG_BENCH {label}: {} events in {:.2} s = {:.0}/s; screen frames {:.0}/s; worker frames {:.0}/s",
            self.events,
            self.seconds,
            self.events as f64 / self.seconds,
            self.frames as f64 / self.seconds,
            self.drawn as f64 / self.seconds,
        );
        println!(
            "DRAG_BENCH {label}: update ms/frame {}",
            stats(&self.update_ms)
        );
        println!(
            "DRAG_BENCH {label}: layout ms/frame {}",
            stats(&self.layout_ms)
        );
        println!(
            "DRAG_BENCH {label}: raster ms/frame {}",
            stats(&self.raster_ms)
        );
        println!(
            "DRAG_BENCH {label}: raster ms/frame with a new worker frame {}",
            stats(&self.fresh_raster_ms)
        );
        println!(
            "DRAG_BENCH {label}: worker ms/frame {}",
            stats(&self.worker_ms)
        );
        let total: Vec<f64> = self
            .update_ms
            .iter()
            .zip(&self.layout_ms)
            .zip(&self.raster_ms)
            .map(|((a, b), c)| a + b + c)
            .collect();
        println!("DRAG_BENCH {label}: main thread ms/frame {}", stats(&total));
    }
}

/// A window over the maps in `dir` (or the Florence fixture), its map
/// canvas `w` x `h` logical pixels at a density of one, at `zoom` over
/// `at`, with its frame at rest drawn.
pub(super) fn window(dir: Option<std::path::PathBuf>, w: f32, h: f32, at: World, zoom: u32) -> App {
    let frames = match dir {
        Some(dir) => Frames::new(loquax::picture::Maps::new(Some(dir))),
        None => map::tests::florence(),
    };
    let maps = frames.painter().epoch();
    let mut app = App::new(frames, None);
    app.screen = Screen::Map;
    app.window = Size::new(w + leviculum_ui_iced::chrome::RAIL, h + 24.0);
    app.seen.maps = maps;
    app.apply(Message::MapSize(Size::new(w, h)));
    app.fitted = true;
    app.view = ExactView::new(at, zoom, 1, 1);
    app.hold();
    // The frame at rest, before the gesture.
    while let Some(job) = app.next_job() {
        match job {
            Job::Draw(order) => {
                let shown = draw(&app.frames, *order);
                app.apply(Message::Painted(shown));
            }
            Job::Prefetch(_) => {
                app.busy = false;
                break;
            }
        }
    }
    app
}

/// A gesture to replay: the message of the `i`-th event, `None` when it
/// is over.
pub(super) type Gesture<'a> = &'a mut dyn FnMut(usize) -> Option<Message>;

/// Replay `gesture` with an event every `every` as iced's loop runs it:
/// the events due folded, the worker's frames taken in, and the screen
/// rasterised (`raster`) whenever something changed or the window asks to
/// be redrawn ([`App::animating`]). The worker is a thread, as iced's
/// executor's pool is.
pub(super) fn replay(app: &mut App, every: Duration, gesture: Gesture<'_>, raster: bool) -> Bench {
    let (job_tx, job_rx) = mpsc::channel::<Order>();
    let (done_tx, done_rx) = mpsc::channel::<(Arc<Shown>, f64)>();
    let frames = app.frames.clone();
    let worker = std::thread::spawn(move || {
        for order in job_rx {
            let start = Instant::now();
            let shown = draw(&frames, order);
            if done_tx
                .send((shown, start.elapsed().as_secs_f64() * 1e3))
                .is_err()
            {
                break;
            }
        }
    });
    let dispatch = |app: &mut App, bench: &mut Bench| match app.next_job() {
        Some(Job::Draw(order)) => {
            if std::env::var_os("LOQUAX_DRAG_BENCH_NO_WORKER").is_none() {
                let _ = job_tx.send(*order);
            }
        }
        Some(Job::Prefetch(_)) => app.busy = false,
        None => {
            let _ = bench;
        }
    };
    let mut screen = raster.then(|| {
        let (w, h) = (app.window.width as u32, app.window.height as u32);
        leviculum_ui_iced::headless::Offscreen::new(w, h).expect("the software renderer")
    });
    let mut bench = Bench::default();
    let start = Instant::now();
    let mut next = 0usize;
    let mut over = false;
    loop {
        let now = start.elapsed();
        let mut changed = false;
        let update = Instant::now();
        while !over && every * next as u32 <= now {
            match gesture(next) {
                Some(message) => {
                    app.apply(message);
                    app.tick(Instant::now());
                    dispatch(app, &mut bench);
                    bench.events += 1;
                    next += 1;
                    changed = true;
                }
                None => over = true,
            }
        }
        let mut fresh = false;
        while let Ok((shown, ms)) = done_rx.try_recv() {
            app.apply(Message::Painted(shown));
            dispatch(app, &mut bench);
            bench.drawn += 1;
            bench.worker_ms.push(ms);
            fresh = true;
        }
        if app
            .wheel
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(WHEEL_REST_MS))
        {
            app.apply(Message::WheelRest);
            dispatch(app, &mut bench);
            changed = true;
        }
        if app.animating() {
            app.tick(Instant::now());
            dispatch(app, &mut bench);
            changed = true;
        }
        let update = update.elapsed().as_secs_f64() * 1e3;
        let idle = !app.busy || std::env::var_os("LOQUAX_DRAG_BENCH_NO_WORKER").is_some();
        if over && idle && !app.animating() && app.wheel.is_none() {
            break;
        }
        if !(changed || fresh) {
            std::thread::sleep(Duration::from_micros(500));
            continue;
        }
        if let Some(screen) = &mut screen {
            let (laid, rastered) = screen.frame(
                app.view(),
                &app.theme(),
                chat::colour(app.tokens().text),
                chat::colour(app.tokens().background),
            );
            let raster_ms = rastered.as_secs_f64() * 1e3;
            bench.layout_ms.push(laid.as_secs_f64() * 1e3);
            bench.raster_ms.push(raster_ms);
            if fresh {
                bench.fresh_raster_ms.push(raster_ms);
            }
        }
        bench.update_ms.push(update);
        bench.frames += 1;
        // A window draws at most once per frame interval.
        let spent = start.elapsed() - now;
        if let Some(rest) = FRAME.checked_sub(spent) {
            std::thread::sleep(rest);
        }
    }
    bench.seconds = start.elapsed().as_secs_f64();
    drop(job_tx);
    let _ = worker.join();
    bench
}

/// A horizontal drag from the canvas's middle, `step` pixels an event,
/// `n` events.
pub(super) fn drag(app: &App, step: f32, n: usize) -> impl FnMut(usize) -> Option<Message> {
    let size = app.size.unwrap_or(Size::new(400.0, 300.0));
    let (x, y) = (size.width * 0.75, size.height / 2.0);
    move |i| match i {
        0 => Some(Message::Pointer(Pointer::Down(Point::new(x, y)))),
        i if i <= n => Some(Message::Pointer(Pointer::Move(Point::new(
            x - step * i as f32,
            y,
        )))),
        i if i == n + 1 => Some(Message::Pointer(Pointer::Up)),
        _ => None,
    }
}

/// With `LOQUAX_DRAG_BENCH` set to a maps directory (`just map-bench`'s
/// `target/map-bench/maps` holds Frankfurt): a drag of two seconds over
/// Frankfurt at z14 in a map canvas of 1600 x 1000, at 125 and at 1000
/// events a second, each a pixel or eight, then the wheel's notches, the
/// numbers printed. A measuring tool for the reviewer:
/// `LOQUAX_DRAG_BENCH=target/map-bench/maps cargo test --release -p
/// loquax --features gui --bins drag_bench -- --nocapture`. Without it,
/// the test returns at once.
#[test]
fn drag_bench() {
    let Some(dir) = std::env::var_os("LOQUAX_DRAG_BENCH") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let at = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_degrees(
        50.1109, 8.6821,
    ));
    let (w, h) = (1600.0, 1000.0);
    for (rate, step) in [(125u64, 8.0f32), (1000, 1.0)] {
        let mut app = window(Some(dir.clone()), w, h, at, 14 * MILLI);
        let every = Duration::from_micros(1_000_000 / rate);
        let n = (2 * rate) as usize;
        let mut gesture = drag(&app, step, n);
        let bench = replay(&mut app, every, &mut gesture, true);
        bench.say(&format!("drag {rate}/s {step}px"));
    }
    // Five notches, a notch every 120 ms, about the canvas's middle.
    let mut app = window(Some(dir), w, h, at, 14 * MILLI);
    let mut wheel = |i: usize| match i {
        i if i < 5 * 15 => Some(match i % 15 {
            0 => Message::Pointer(Pointer::Wheel(Point::new(w / 2.0, h / 2.0), 1000)),
            _ => Message::Pointer(Pointer::Move(Point::new(w / 2.0, h / 2.0))),
        }),
        _ => None,
    };
    let bench = replay(&mut app, Duration::from_millis(8), &mut wheel, true);
    bench.say("wheel 5 notches");
    // The control: the same screen with nothing on it, the read back of
    // its pixels alone.
    let mut screen = leviculum_ui_iced::headless::Offscreen::new(
        app.window.width as u32,
        app.window.height as u32,
    )
    .expect("the software renderer");
    let mut empty = Vec::new();
    for _ in 0..30 {
        let (_, r) = screen.frame(
            Element::<'_, Message>::from(Space::new()),
            &app.theme(),
            Color::BLACK,
            Color::WHITE,
        );
        empty.push(r.as_secs_f64() * 1e3);
    }
    println!("DRAG_BENCH empty screen raster ms {}", stats(&empty));
}

/// A drag asks no frame per event (1431): sixty moves of two pixels over
/// the Florence fixture, an event a millisecond as a fast mouse sends
/// them, are sixty-two events in; the screen shows the frame on screen
/// moved with each, and the worker is asked again only when that frame's
/// margin runs short, at most once a frame interval: a frame or a few
/// while the view moves, not sixty. The pointer's release asks the frame
/// at rest, at the view the drag left. The control: the view moved by
/// every pixel of the drag, so the frame at rest it began on could not
/// have served throughout.
#[test]
fn a_drag_asks_no_frame_per_event() {
    let at = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_degrees(
        43.7752, 11.2660,
    ));
    let mut app = window(None, 480.0, 320.0, at, 16 * MILLI);
    let before = (app.trace.events, app.trace.asked_moving);
    let rest = app.shown.clone().expect("the frame at rest");
    let mut gesture = drag(&app, 2.0, 60);
    let bench = replay(&mut app, Duration::from_millis(1), &mut gesture, false);
    assert_eq!(bench.events, 62);
    assert_eq!(app.trace.events - before.0, 62);
    // 120 pixels against margins of 60 a side: asked again once 30 of
    // them are used.
    let moving = app.trace.asked_moving - before.1;
    assert!((1..=4).contains(&moving), "{moving} frames for 62 events");
    assert!(bench.frames >= 1, "the screen drew");
    let last = app.shown.clone().expect("a frame");
    assert!(!last.order.moving, "the last frame is at rest");
    assert_eq!(last.order.view, app.view, "at the view the drag left");
    let (x0, _, _) = onto(&rest.view, &app.view);
    let (x1, _, _) = onto(&rest.view, &rest.order.view);
    assert!((x1 - x0 - 120.0).abs() < 1.0, "{x0} {x1}");
}

/// The wheel's notch glides (1431): over [`GLIDE_MS`] the view's zoom
/// climbs from the zoom on screen to the one the notch asks, never
/// back, eased out (more of it in the first half than in the second),
/// the world point under the pointer under it all the way; two notches
/// in quick succession go to two steps; a touchpad's ten turns of a
/// tenth add up to one step exactly, the same path.
#[test]
fn the_wheel_glides_to_its_zoom() {
    let at = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_degrees(
        43.7752, 11.2660,
    ));
    let mut app = window(None, 480.0, 320.0, at, 14 * MILLI);
    let (px, py) = (120.0, 90.0);
    let held = app.view.to_world(px, py);
    app.apply(Message::Pointer(Pointer::Wheel(
        Point::new(120.0, 90.0),
        1000,
    )));
    let start = app.glide.expect("a glide").start;
    let mut last = 14_000;
    let mut half = 0;
    for ms in (0..=GLIDE_MS).step_by(15) {
        app.tick(start + Duration::from_millis(ms));
        let z = app.view.zoom_milli();
        assert!(z >= last, "{ms} ms: {z} after {last}");
        last = z;
        if ms == GLIDE_MS / 2 {
            half = z;
        }
        let (x, y) = app.view.to_screen(held);
        assert!(
            (x - px).abs() < 1.0 && (y - py).abs() < 1.0,
            "{ms} ms: {x} {y}"
        );
    }
    assert_eq!(last, 15_000);
    assert!(half > 14_500, "eased out: {half} at half time");
    assert!(app.glide.is_none());
    // Two notches before the first has glided.
    app.apply(Message::Pointer(Pointer::Wheel(
        Point::new(120.0, 90.0),
        1000,
    )));
    app.tick(Instant::now() + Duration::from_millis(30));
    app.apply(Message::Pointer(Pointer::Wheel(
        Point::new(120.0, 90.0),
        1000,
    )));
    assert_eq!(app.glide.expect("a glide").to.zoom_milli(), 17_000);
    app.tick(Instant::now() + Duration::from_millis(GLIDE_MS + 1));
    assert_eq!(app.view.zoom_milli(), 17_000);
    // A touchpad's tenths.
    for _ in 0..10 {
        app.apply(Message::Pointer(Pointer::Wheel(
            Point::new(120.0, 90.0),
            -100,
        )));
        app.tick(Instant::now() + Duration::from_millis(5));
    }
    app.tick(Instant::now() + Duration::from_millis(GLIDE_MS + 1));
    assert_eq!(app.view.zoom_milli(), 16_000);
    let (x, y) = app.view.to_screen(held);
    assert!((x - px).abs() < 1.0 && (y - py).abs() < 1.0, "{x} {y}");
}
