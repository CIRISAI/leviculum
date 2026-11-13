//! The render walk's rules on real frames (1801): the Florence view drawn
//! through the facade's own call into a Bitmap sized as `MapView.java`
//! sizes it. The frame fits and covers the screen; a Bitmap of the 1796
//! fault's size (half again each side) fails `bitmap_is_frame`, and the
//! screen Java keeps then is the last picture shrunk, which fails
//! `view_covered`; the settled frame's names are inside the frame. A test
//! binary of its own: the facade keeps a renderer per directory.

use std::path::PathBuf;

use leviculum_map::mercator::{project, LatLon};
use leviculum_map::outline::space;
use leviculum_mapfill::frame::rgb888;
use leviculum_mapfill::style::MapTheme;
use loquax::when::WallTime;
use loquax_android::map::{drawn_names, hold_rung, render_into};
use loquax_android::render_walk::{
    frame_rules, java_margin, FrameFacts, JavaBitmap, Screen, Verdict, BITMAP_IS_FRAME,
    NO_ERROR_TEXT, SENTINEL, VIEW_COVERED,
};
use loquax_android::schema::MapRequest;

const W: u32 = 480;
const H: u32 = 320;

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
    hold_rung(home.path(), Some(0));
    home
}

fn ask(home: &tempfile::TempDir) -> MapRequest {
    let at = project(LatLon::from_degrees(43.7752, 11.2660));
    MapRequest {
        dir: home.path().display().to_string(),
        w: W,
        h: H,
        density_milli: 1000,
        x: Some(at.x),
        y: Some(at.y),
        zoom: Some(16),
        margin_x: java_margin(W, false),
        margin_y: java_margin(H, false),
        ..MapRequest::default()
    }
}

fn now() -> WallTime {
    WallTime {
        unix_secs: 1_790_000_000,
        offset_secs: 0,
    }
}

fn verdict(v: &[Verdict], rule: &str) -> Verdict {
    v.iter().find(|v| v.rule == rule).cloned().expect(rule)
}

#[test]
fn a_frame_in_the_java_bitmap_fits_and_covers_and_the_1796_bitmap_does_not() {
    let home = home();
    let ask = ask(&home);
    let (bw, bh) = (W + 2 * ask.margin_x, H + 2 * ask.margin_y);
    let mut bitmap = JavaBitmap::new(bw, bh);
    let shown = render_into(&ask, Some(Vec::new()), now(), &mut bitmap);
    assert!(shown.ok, "{:?}", shown.error);
    assert_eq!((shown.w, shown.h), (bw, bh));
    let screen = Screen::landed(
        &bitmap.rgba,
        shown.w,
        (shown.margin_x, shown.margin_y),
        (W, H),
        shown.zoom_milli,
    );
    assert!(
        screen.rgba.chunks_exact(4).all(|px| px != SENTINEL),
        "the frame draws every pixel of the screen"
    );
    let names = drawn_names(home.path()).expect("a renderer");
    assert!(!names.is_empty(), "Florence at z16 has names");
    let space = rgb888(space(MapTheme::Light));
    let facts = |shown, screen| {
        frame_rules(&FrameFacts {
            shown,
            screen,
            view: (W, H),
            density: 1.0,
            names: &names,
            settled: true,
            space,
        })
    };
    let good = facts(&shown, &screen);
    for rule in [BITMAP_IS_FRAME, VIEW_COVERED, NO_ERROR_TEXT] {
        assert!(verdict(&good, rule).pass, "{:?}", verdict(&good, rule));
    }
    // The Bitmap of 1796's fault: half again the frame on each side.
    let mut wrong = JavaBitmap::new(bw * 3 / 2, bh * 3 / 2);
    let failed = render_into(&ask, Some(Vec::new()), now(), &mut wrong);
    assert!(!failed.ok);
    assert_eq!(
        failed.error.as_deref(),
        Some(
            format!(
                "map: the bitmap is {}x{}, the frame {bw}x{bh}",
                bw * 3 / 2,
                bh * 3 / 2
            )
            .as_str()
        )
    );
    assert!(
        wrong.rgba.chunks_exact(4).all(|px| px == SENTINEL),
        "nothing was drawn into it"
    );
    // Java shows the last picture scaled to the zoom asked: two steps
    // out, a sixteenth of the screen.
    let shrunk = screen.failed((W, H), shown.zoom_milli - 2000);
    let bad = facts(&failed, &shrunk);
    let fit = verdict(&bad, BITMAP_IS_FRAME);
    assert!(!fit.pass && fit.note.contains("the bitmap is"), "{fit:?}");
    let cover = verdict(&bad, VIEW_COVERED);
    assert!(!cover.pass && cover.value > 90.0, "{cover:?}");
    assert!(
        verdict(&bad, NO_ERROR_TEXT).pass,
        "the misfit is its own rule, not an error text"
    );
}
