//! The browser's scene (1255, slice 1 of
//! `docs/src/concepts/lnomad-everywhere.md`) of the Reticulum README, the
//! largest page in the tree, pinned as JSON under
//! `tests/golden/scene-page-{53,80}.json`: the M9's 53 columns and a
//! terminal's 80, 24 rows each. Keys sorted, texts as catalogue keys with
//! their arguments, so one file pins both languages' structure, and the
//! page as the runs of the one layout every target draws.
//!
//! A scene that moved fails with the first differing line and leaves the
//! new one in the temp dir as `lnomad-golden/<name>`;
//! `LNOMAD_WRITE_GOLDEN=1` writes the goldens instead, for a change that
//! means to move them.

use std::path::PathBuf;

use leviculum_micron::parse;
use lnomad::app::scene::Scene;
use lnomad::app::{update, AppEvent, Focus, KeyCode, KeyEvent, KeyModifiers};
use lnomad::url::parse_url;
use lnomad::Model;

const README: &str = include_str!("../tests_data/reticulum_readme.mu");

/// A node the README is served from.
const NODE: &str = "c0ffee00112233445566778899aabbcc";

/// The README as a terminal `cols` by 24 shows it after a load: the page
/// in history, its node known, the second link focused by two Tabs.
fn readme(cols: u16) -> Model {
    let size = (cols, 24);
    let doc = parse(README);
    let mut model = Model::from_document(&doc, (cols - 1) as usize, "Reticulum · README", size);
    let target = parse_url(&format!("{NODE}:/page/readme.mu"), None).expect("url");
    model.current_dest = Some(target.dest_hash);
    model.history.visit(target);
    for _ in 0..2 {
        update(
            &mut model,
            AppEvent::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        );
    }
    model
}

fn json(scene: &Scene) -> String {
    let mut got = serde_json::to_string_pretty(&scene.to_json()).expect("json");
    got.push('\n');
    got
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Where `got` differs from the golden `name`, or `None`.
fn differs(name: &str, got: &str) -> Option<String> {
    let want = std::fs::read_to_string(golden(name)).unwrap_or_default();
    if got == want {
        return None;
    }
    let line = got
        .lines()
        .zip(want.lines())
        .position(|(g, w)| g != w)
        .unwrap_or_else(|| got.lines().count().min(want.lines().count()));
    Some(format!(
        "{name} moved at line {}: got {:?}, want {:?}",
        line + 1,
        got.lines().nth(line),
        want.lines().nth(line)
    ))
}

fn check(name: &str, scene: &Scene) {
    let got = json(scene);
    if std::env::var_os("LNOMAD_WRITE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden("")).expect("the golden dir");
        std::fs::write(golden(name), &got).expect("write the golden");
    }
    if let Some(why) = differs(name, &got) {
        let dir = std::env::temp_dir().join("lnomad-golden");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), &got);
        panic!("{why}");
    }
}

#[test]
fn the_readme_is_the_golden_scene_at_53_and_80_columns() {
    for cols in [53, 80] {
        let model = readme(cols);
        let scene = model.scene();
        assert_eq!(scene.page.focus, Some(Focus::Link(2)));
        assert_eq!(
            scene.page.lines.len(),
            22,
            "the window is the screen's rows"
        );
        check(&format!("scene-page-{cols}.json"), &scene);
    }
}

/// The positive control: the golden sees a run's colour and a link's
/// place. A scene with either changed is refused.
#[test]
fn a_changed_run_colour_or_a_moved_link_fails_the_golden() {
    let name = "scene-page-53.json";
    let model = readme(53);
    let scene = model.scene();
    assert_eq!(differs(name, &json(&scene)), None, "the golden itself");

    let mut recoloured = scene.clone();
    let run = recoloured
        .page
        .lines
        .iter_mut()
        .flat_map(|line| line.runs.iter_mut())
        .find(|run| run.link.is_some())
        .expect("a link run");
    run.style.fg = Some(lnomad::Ink::Rgb(1, 2, 3));
    assert!(
        differs(name, &json(&recoloured)).is_some(),
        "a run's colour"
    );

    // The link one cell to the right: a blank run before it grows by one.
    let mut moved = scene.clone();
    let line = moved
        .page
        .lines
        .iter_mut()
        .find(|line| line.runs.iter().any(|run| run.link.is_some()))
        .expect("a line with a link");
    let at = line
        .runs
        .iter()
        .position(|run| run.link.is_some())
        .expect("the link run");
    match at {
        0 => line.runs[0].text.insert(0, ' '),
        _ => line.runs[at - 1].text.push(' '),
    }
    assert!(differs(name, &json(&moved)).is_some(), "a link's place");
}

/// The most runs in any window of the README laid out `width` cells wide.
fn most_runs(width: u16) -> usize {
    // The terminal keeps one column for its scrollbar.
    let mut model = readme(width + 1);
    let vp = model.viewport();
    assert_eq!(vp, 22);
    let mut most = 0;
    for first in 0..=model.max_scroll(vp) {
        model.scroll = first;
        most = most.max(model.scene().page.runs());
    }
    most
}

/// Section 2.2 measured at most 67 runs in a window of the README laid
/// out on the M9's 53 cells, 22 rows; the scene holds to that on every
/// scroll position. The terminal 53 columns wide lays out 52, one cell
/// less for its scrollbar, which wraps one line more and peaks at 68.
#[test]
fn a_window_of_the_readme_at_53_cells_holds_at_most_67_runs() {
    assert_eq!(most_runs(53), 67);
    assert_eq!(most_runs(52), 68);
}

/// A scene of the README is built in well under a frame: the number goes
/// into the plan's page (slice 1, "Accepted by").
#[test]
fn a_readme_scene_is_built_in_well_under_a_frame() {
    let model = readme(53);
    let rounds = 200;
    let start = std::time::Instant::now();
    for _ in 0..rounds {
        std::hint::black_box(model.scene());
    }
    let each = start.elapsed() / rounds;
    eprintln!("a README scene at 53x24: {each:?} per build");
    assert!(each < std::time::Duration::from_millis(16), "{each:?}");
}
