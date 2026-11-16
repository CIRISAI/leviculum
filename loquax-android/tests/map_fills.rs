//! The phone's map fills its frame (1796): the Bitmap Java hands the
//! library is the frame the library draws, at every density, at world
//! zooms where the globe turns and deeper where the map pans, and after a
//! rotation; and the world at its smallest is never smaller than the view.
//!
//! On 2026-11-13 the globe build drew nothing past its first globe frame:
//! Java sized the Bitmap with the margins it asked for, the library drew a
//! turning globe without them (1762), and every frame failed with "map:
//! the bitmap is 1836x3295, the frame 1224x2197", the last flat frame
//! shrinking under the pinch in the middle of an empty screen.

use std::path::{Path, PathBuf};

use leviculum_map::blend::{Blend, Shape};
use leviculum_map::mercator::{project, LatLon};
use loquax::when::WallTime;
use loquax_android::map::Canvas;
use loquax_android::schema::{MapRequest, MapShown};

/// The densities the order names, and the screen of Lew's phone in
/// pixels (1224 x 2197 under the bars).
const DENSITIES: [u32; 4] = [1000, 2000, 2750, 3000];
const PHONE: (u32, u32) = (1224, 2197);

/// The map frames of the render walk of 2026-11-13 (1797, finding 4) at
/// their densities: the phone's tab and its map screen, and the tablet
/// of 1600 x 2560 at 320 dpi, upright and turned, whose Equal Earth
/// minimum showed the world smaller than the screen.
const WALK_FRAMES: [(u32, (u32, u32)); 4] = [
    (2750, (1224, 1702)),
    (2750, (1224, 1878)),
    (2000, (1600, 2184)),
    (2000, (2184, 1600)),
];

/// The share of the screen Java asks as a margin on each side
/// (`MapView.OVERSCAN`).
const OVERSCAN: f64 = 0.25;

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

/// A Bitmap as Java makes one: its size fixed before the call, the size
/// every `pixels` call asked for kept.
struct Bitmap {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    asked: Vec<(u32, u32)>,
}

impl Bitmap {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            rgba: vec![0; w as usize * h as usize * 4],
            asked: Vec::new(),
        }
    }
}

impl Canvas for Bitmap {
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String> {
        self.asked.push((w, h));
        if (w, h) != (self.w, self.h) {
            return Err(format!(
                "map: the bitmap is {}x{}, the frame {w}x{h}",
                self.w, self.h
            ));
        }
        Ok(&mut self.rgba)
    }
}

/// Java's margins for a `w` x `h` screen: a quarter on each side, none
/// while the last frame turned the globe.
fn margins(w: u32, h: u32, turns: bool) -> (u32, u32) {
    match turns {
        true => (0, 0),
        false => (
            (f64::from(w) * OVERSCAN).round() as u32,
            (f64::from(h) * OVERSCAN).round() as u32,
        ),
    }
}

/// A frame of `request` into a Bitmap sized as Java sizes it from the
/// request's margins; the Bitmap must be the frame.
fn draw(request: &MapRequest) -> MapShown {
    let (w, h) = (
        request.w + 2 * request.margin_x,
        request.h + 2 * request.margin_y,
    );
    let mut bitmap = Bitmap::new(w, h);
    let shown = loquax_android::map::render_into(request, Some(Vec::new()), now(), &mut bitmap);
    assert!(
        shown.ok,
        "{}x{} at {}: {:?}",
        request.w, request.h, request.density_milli, shown.error
    );
    assert_eq!(bitmap.asked, vec![(w, h)], "the frame is the Bitmap");
    assert_eq!((shown.w, shown.h), (w, h));
    assert_eq!(
        (shown.margin_x, shown.margin_y),
        (request.margin_x, request.margin_y),
        "the margins drawn are the margins asked"
    );
    shown
}

/// A request over Florence for a `w` x `h` screen at `zoom_milli`.
fn ask(home: &Path, (w, h): (u32, u32), density: u32, zoom_milli: u32) -> MapRequest {
    let at = project(LatLon::from_degrees(43.7752, 11.2660));
    MapRequest {
        dir: home.display().to_string(),
        w,
        h,
        density_milli: density,
        x: Some(at.x),
        y: Some(at.y),
        zoom_milli: Some(zoom_milli),
        ..MapRequest::default()
    }
}

/// The screens to try at `density`: Lew's phone, a phone of 360 x 800 dp
/// at that density, each in both orientations.
fn screens(density: u32) -> Vec<(u32, u32)> {
    let dp = |v: u32| v * density / 1000;
    let mut out = Vec::new();
    for (w, h) in [PHONE, (dp(360), dp(800))] {
        out.push((w, h));
        out.push((h, w));
    }
    out
}

/// At every density, in both orientations, at a world zoom (the globe
/// turns there in the globe build), between the steps and deep in the
/// region, with Java's margins and with none: the Bitmap Java sized from
/// the request is the frame the library draws, never a raw size line.
#[test]
fn the_bitmap_is_the_frame_at_every_density() {
    let home = home();
    for density in DENSITIES {
        for screen in screens(density) {
            for zoom in [0, 4_200, 14_000] {
                for turns in [false, true] {
                    let (mx, my) = margins(screen.0, screen.1, turns);
                    draw(&MapRequest {
                        margin_x: mx,
                        margin_y: my,
                        ..ask(home.path(), screen, density, zoom)
                    });
                }
            }
        }
    }
}

/// The phone's frames as Java asks them: the margins of the last frame's
/// `turns`. A pinch out from the region to the world, then a rotation:
/// each frame's Bitmap is the frame, the new size after the rotation
/// included, where Java makes a new Bitmap because the old one no longer
/// fits.
#[test]
fn a_size_change_between_two_frames_draws_into_the_new_bitmap() {
    let home = home();
    let mut turns = false;
    let mut screen = PHONE;
    let mut last: Option<MapShown> = None;
    for (zoom_by_milli, rotate) in [
        (0, false),
        (-4_000, false),
        (-4_000, false),
        (-2_000, false),
        (0, true),
        (500, false),
        (0, true),
    ] {
        if rotate {
            screen = (screen.1, screen.0);
        }
        let (mx, my) = margins(screen.0, screen.1, turns);
        let mut request = MapRequest {
            margin_x: mx,
            margin_y: my,
            zoom_by_milli,
            about_x: (screen.0 / 2) as i32,
            about_y: (screen.1 / 2) as i32,
            ..ask(home.path(), screen, 2750, 14_000)
        };
        if let Some(shown) = &last {
            request.x = Some(shown.x);
            request.y = Some(shown.y);
            request.zoom_milli = Some(shown.zoom_milli);
        }
        let shown = draw(&request);
        turns = shown.turns;
        last = Some(shown);
    }
}

/// How wide the world `shape` is drawn at `zoom` over a `w` x `h` view
/// centred at `centre`, in pixels: the globe's disk, Equal Earth's
/// equator from edge meridian to edge meridian, both at the blend the
/// frame is drawn by.
fn world_across(shape: Shape, shown: &MapShown, w: u32, h: u32) -> f64 {
    let centre = leviculum_map::mercator::World {
        x: shown.x,
        y: shown.y,
    };
    let zoom = f64::from(shown.zoom_milli) / 1000.0;
    let blend = Blend::new(shape, centre, zoom, w, h);
    match shape {
        Shape::Globe => {
            assert_eq!(blend.t(), 0.0, "the smallest world is the globe itself");
            2.0 * blend.radius()
        }
        Shape::EqualEarth => {
            let pi = std::f64::consts::PI;
            let (west, _) = blend.to_screen_angles(-pi, 0.0);
            let (east, _) = blend.to_screen_angles(pi, 0.0);
            east - west
        }
    }
}

/// The phone zoomed out as far as it goes (1796, Lew's second
/// screenshot): at every density and in both orientations the world
/// covers the view's width, the globe its shorter side, and one more
/// step out changes nothing.
#[test]
fn the_world_at_its_smallest_covers_the_view() {
    let home = home();
    let shape = loquax::picture::world_shape();
    let mut cases: Vec<(u32, (u32, u32))> = DENSITIES
        .iter()
        .flat_map(|&d| screens(d).into_iter().map(move |s| (d, s)))
        .collect();
    cases.extend(WALK_FRAMES);
    for (density, (w, h)) in cases {
        let out = |from: Option<&MapShown>| {
            let mut request = MapRequest {
                zoom_by_milli: -20_000,
                about_x: (w / 2) as i32,
                about_y: (h / 2) as i32,
                ..ask(home.path(), (w, h), density, 14_000)
            };
            if let Some(shown) = from {
                request.x = Some(shown.x);
                request.y = Some(shown.y);
                request.zoom_milli = Some(shown.zoom_milli);
                request.zoom_by_milli = -1_000;
            }
            let (mx, my) = margins(w, h, false);
            draw(&MapRequest {
                margin_x: mx,
                margin_y: my,
                ..request
            })
        };
        let smallest = out(None);
        let across = world_across(shape, &smallest, w, h);
        let side = match shape {
            Shape::Globe => w.min(h),
            Shape::EqualEarth => w,
        };
        assert!(
            across >= f64::from(side) - 0.5,
            "{w}x{h} at {density}: the world {across:.1} px across, the view {side}"
        );
        if shape == Shape::Globe && w < h {
            assert!(
                across <= f64::from(side) + 2.0,
                "{w}x{h}: the globe's diameter is the view's width, not {across:.1}"
            );
        }
        assert_eq!(
            smallest.zoom_milli, smallest.min_zoom_milli,
            "{w}x{h}: Java is told the floor the pinch stops at"
        );
        let again = out(Some(&smallest));
        assert_eq!(
            again.zoom_milli, smallest.zoom_milli,
            "{w}x{h}: a step further out changes nothing"
        );
        assert_eq!((again.x, again.y), (smallest.x, smallest.y));
    }
}
