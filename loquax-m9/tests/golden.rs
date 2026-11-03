//! The M9's chats list drawn on the host (1351, `handheld-messenger.md`,
//! "The test plan", item 1): the ugly store's model (1173), its Compact
//! scene, drawn by the M9's renderer through mousefood into the 320 x 240
//! RGB565 frame, and pinned as a PNG under `tests/golden/`, compared in
//! bytes. A frame that moved fails and leaves the new picture beside the
//! test's target dir as `m9-golden/<name>`; `LOQUAX_WRITE_GOLDEN=1` writes
//! the goldens instead, for a change that means to move them.
//!
//! The board dumps the same frame over its debug link and the rig compares
//! it with the same file (item 3, a later slice).

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

use std::path::{Path, PathBuf};

use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::store::Store;
use loquax::ui::scene::{LayoutClass, Scene};
use loquax::ui::strings::Lang;
use loquax::ui::{update, UiEvent, UiModel, NONE_OPEN};
use loquax::when::WallTime;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::render::Renderer;
use loquax_m9::screen::{Look, ENTRY_ROWS};

/// A compose field nobody types into.
#[derive(Debug, Default, Clone)]
struct Line(String);

impl InputBuffer for Line {
    fn value(&self) -> &str {
        &self.0
    }
    fn cursor(&self) -> usize {
        self.0.chars().count()
    }
    fn edit(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
    fn newline(&mut self) {
        self.0.push('\n');
    }
    fn set_cursor(&mut self, _at: usize) {}
    fn set_value(&mut self, text: &str) {
        self.0 = text.to_string();
    }
}

/// The reader: no conversation's peer.
const OWN: usize = 0x30;

/// A day after the store's last row: every time in the list is fixed.
fn now() -> WallTime {
    let last = ugly_store::BASE
        + (ugly_store::PER_CONVERSATION as i64) * 600
        + (ugly_store::CONVERSATIONS as i64) * 7;
    WallTime {
        unix_secs: last + 86_400,
        offset_secs: 3600,
    }
}

/// The model a loquax restores from the ugly store, as the shell builds it.
fn ugly_model(home: &Path) -> UiModel<Line> {
    let own = ugly_store::identity(OWN);
    ugly_store::fill_store_of(home, &own);
    let me = loquax::engine::delivery_address(&own).expect("address");
    let store = Store::open_in(home, me).expect("the store");
    let contacts = loquax::names::load_contacts(home).expect("contacts");
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some(me);
    model.names = loquax::names::Names::new(contacts);
    for (peer, name) in store.names().expect("names") {
        model.names.remember(peer, &name);
    }
    update(&mut model, UiEvent::Clock(now()));
    model.restore(store.restore().expect("restore"));
    model.restore_positions(store.positions().expect("positions"));
    model
}

/// The chats list at the M9's class, nothing open.
fn list_scene() -> Scene {
    let home = tempfile::tempdir().expect("home");
    let mut model = ugly_model(home.path());
    model.selected = NONE_OPEN;
    model.scene(LayoutClass::Compact)
}

/// `scene` with the list's cursor on row `at`, as the M9's list will carry
/// it: past the first screen, so the list scrolls.
fn with_cursor(mut scene: Scene, at: usize) -> Scene {
    let list = scene.list.as_mut().expect("the list");
    for (i, row) in list.rows.iter_mut().enumerate() {
        row.item.selected = i == at;
    }
    scene
}

fn look() -> Look {
    Look::m9(Lang::En, Some(now()))
}

/// `scene` drawn into a fresh frame by a fresh renderer.
fn draw(scene: &Scene) -> Frame {
    let mut frame = Frame::new();
    Renderer::new(&mut frame)
        .expect("renderer")
        .draw(scene, &look())
        .expect("draw");
    frame
}

fn png(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer
        .write_image_data(&frame.to_rgb888())
        .expect("png data");
    writer.finish().expect("png end");
    out
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check(name: &str, frame: &Frame) {
    let got = png(frame);
    let path = golden(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read(&path).unwrap_or_default();
    if got != want {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m9-golden");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), &got);
        panic!(
            "{name} moved: {} B drawn against {} B pinned; the drawn one is {}",
            got.len(),
            want.len(),
            dir.join(name).display()
        );
    }
}

/// The pixel rows two frames differ in, top and bottom.
fn changed_rows(a: &Frame, b: &Frame) -> Option<(usize, usize)> {
    let w = WIDTH as usize;
    let rows: Vec<usize> = (0..HEIGHT as usize)
        .filter(|&y| a.pixels()[y * w..(y + 1) * w] != b.pixels()[y * w..(y + 1) * w])
        .collect();
    Some((*rows.first()?, *rows.last()?))
}

#[test]
fn the_chats_list_is_the_golden_frame() {
    let scene = list_scene();
    assert!(
        scene.list.as_ref().is_some_and(|l| l.rows.len() > 11),
        "more conversations than one screen holds, so the cursor scrolls"
    );
    check("m9-chats.png", &draw(&scene));
    check("m9-chats-cursor.png", &draw(&with_cursor(scene, 13)));
}

/// The positive control: one row's time moved two days back changes the
/// frame, and only in that row's band of pixels, so the byte comparison
/// above sees a moved timestamp and the bands are where the layout says.
#[test]
fn a_moved_timestamp_moves_its_row_and_nothing_else() {
    let scene = list_scene();
    let mut moved = scene.clone();
    let row = 3;
    let list = moved.list.as_mut().expect("the list");
    let at = list.rows[row].item.id;
    let when = list.rows[row].row.at.as_mut().expect("the row's time");
    when.unix_secs -= 2 * 86_400;
    let (top, bottom) = changed_rows(&draw(&scene), &draw(&moved))
        .unwrap_or_else(|| panic!("moving {at:?}'s time changed no pixel"));
    // The top bar is one cell row of 10 px, each entry ENTRY_ROWS more.
    let band = (10 + row * ENTRY_ROWS * 10)..(10 + (row + 1) * ENTRY_ROWS * 10);
    assert!(
        band.contains(&top) && band.contains(&bottom),
        "the change is in pixel rows {top}..={bottom}, the row's band is {band:?}"
    );
}

/// The renderer is made once on the board and redraws only what changed:
/// moving the cursor with one renderer gives the frame a fresh one draws.
#[test]
fn a_redraw_is_the_frame_a_fresh_draw_gives() {
    let scene = list_scene();
    let mut frame = Frame::new();
    {
        let mut renderer = Renderer::new(&mut frame).expect("renderer");
        renderer
            .draw(&with_cursor(scene.clone(), 2), &look())
            .expect("first draw");
        renderer
            .draw(&with_cursor(scene.clone(), 13), &look())
            .expect("second draw");
    }
    let fresh = draw(&with_cursor(scene, 13));
    assert!(
        frame.pixels() == fresh.pixels(),
        "the redraw differs in pixel rows {:?}",
        changed_rows(&frame, &fresh)
    );
}
