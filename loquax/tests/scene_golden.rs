//! The scene of the list and the chat (1217), built from the ugly store
//! (1173) for both layout classes and pinned as JSON under
//! `tests/golden/scene-*.json`: keys sorted, texts as catalogue keys with
//! their arguments, so one file pins both languages' structure. A scene
//! that moved fails with the first differing line and leaves the new one
//! in the temp dir as `loquax-golden/<name>`; `LOQUAX_WRITE_GOLDEN=1`
//! writes the goldens instead, for a change that means to move them.

#[path = "support/ugly_store.rs"]
mod ugly_store;

use std::path::{Path, PathBuf};

use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::store::Store;
use loquax::ui::scene::LayoutClass;
use loquax::ui::{update, UiEvent, UiModel, NONE_OPEN};
use loquax::when::WallTime;

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
}

/// The reader: no conversation's peer.
const OWN: usize = 0x30;

/// A day after the store's last row: every age and day line is fixed.
fn now() -> WallTime {
    let last = ugly_store::BASE
        + (ugly_store::PER_CONVERSATION as i64) * 600
        + (ugly_store::CONVERSATIONS as i64) * 7;
    WallTime {
        unix_secs: last + 86_400,
        offset_secs: 3600,
    }
}

/// The model a terminal loquax restores from the ugly store, as the shell
/// builds it: contacts, names, the history, the positions.
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

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check(name: &str, scene: &loquax::ui::scene::Scene) {
    let mut got = serde_json::to_string_pretty(&scene.to_json()).expect("json");
    got.push('\n');
    let path = golden(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if got != want {
        let dir = std::env::temp_dir().join("loquax-golden");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), &got);
        let line = got
            .lines()
            .zip(want.lines())
            .position(|(g, w)| g != w)
            .unwrap_or_else(|| got.lines().count().min(want.lines().count()));
        panic!(
            "{name} moved at line {}: got {:?}, want {:?}",
            line + 1,
            got.lines().nth(line),
            want.lines().nth(line)
        );
    }
}

#[test]
fn the_ugly_store_is_the_golden_scene_in_both_classes() {
    let home = tempfile::tempdir().expect("home");
    let mut model = ugly_model(home.path());
    let open = model
        .conversations
        .iter()
        .position(|c| c.peer == ugly_store::peer(2))
        .expect("conversation 2");
    model.selected = NONE_OPEN;
    check(
        "scene-compact-list.json",
        &model.scene(LayoutClass::Compact),
    );
    model.selected = open;
    check(
        "scene-compact-chat.json",
        &model.scene(LayoutClass::Compact),
    );
    check("scene-wide.json", &model.scene(LayoutClass::Wide));
}

/// The scene is a function of the store: two homes filled alike give the
/// same scene, so the goldens pin no accident of a run.
#[test]
fn the_scene_is_the_same_from_two_homes() {
    let a = tempfile::tempdir().expect("home a");
    let b = tempfile::tempdir().expect("home b");
    let a = ugly_model(a.path()).scene(LayoutClass::Wide).to_json();
    let b = ugly_model(b.path()).scene(LayoutClass::Wide).to_json();
    assert_eq!(a, b);
}
