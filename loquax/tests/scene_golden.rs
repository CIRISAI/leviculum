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
    fn set_value(&mut self, text: &str) {
        self.0 = text.to_string();
    }
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

/// 1307 pass two: the four group screens as every target gets them. A
/// reader who was invited by Lew to "Familie" and joined, a second
/// invitation still open, the group's chat with Lew's and Anna's copies
/// under their names, the member sheet, and "New group" at its two steps.
/// Lew is the reader's contact; the group, its members and the copies come
/// as the home sends them.
#[test]
fn the_group_screens_are_the_golden_scenes() {
    use leviculum_lxmf::lev::{GroupCopy, Invite, Lev, Member, Members};
    use loquax::outbox::OutboxEvent;
    use loquax::ui::groups::Verb;
    use loquax::ui::intent::Target;

    const ME: [u8; 16] = [0x30; 16];
    const LEW: [u8; 16] = [0x41; 16];
    const ANNA: [u8; 16] = [0x42; 16];
    const BOB: [u8; 16] = [0x43; 16];
    const FAMILIE: [u8; 16] = [0x51; 16];
    const VEREIN: [u8; 16] = [0x52; 16];
    let at = |minutes: i64| WallTime {
        unix_secs: 1_800_000_000 + minutes * 60,
        offset_secs: 3600,
    };
    let fields = |lev: Lev| {
        let mut fields = Vec::new();
        lev.put_into(&mut fields);
        fields
    };
    let invite = |group: [u8; 16], name: &str| Lev {
        capabilities: Some(3),
        invite: Some(Invite {
            group,
            public_key: [7; 64],
            name: name.into(),
            admin_name: "Lew".into(),
            epoch: 1,
        }),
        ..Lev::default()
    };
    let copy = |writer: [u8; 16]| Lev {
        capabilities: Some(2),
        group_copy: Some(GroupCopy {
            sender: writer,
            sent_ms: 1,
            epoch: 2,
        }),
        ..Lev::default()
    };
    let mut model = UiModel::<Line>::new(&[BOB]);
    model.address = Some(ME);
    model.names.remember(LEW, "Lew");
    model.names.remember(BOB, "Bob");
    let arrive = |model: &mut UiModel<Line>, minute, from, id: u8, body: &str, lev: Lev| {
        update(model, UiEvent::Clock(at(minute)));
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: fields(lev),
                verified: true,
            }),
        );
    };
    arrive(
        &mut model,
        0,
        LEW,
        1,
        "Lew invited you to Familie: lxma://…",
        invite(FAMILIE, "Familie"),
    );
    update(
        &mut model,
        UiEvent::Group(Verb::Join {
            group: Target::Peer(FAMILIE),
        }),
    );
    let list = |flags: [u64; 3]| Lev {
        capabilities: Some(3),
        members: Some(Members {
            epoch: 2,
            members: vec![
                Member {
                    address: LEW,
                    name: "Lew".into(),
                    flags: flags[0],
                },
                Member {
                    address: ME,
                    name: "Kim".into(),
                    flags: flags[1],
                },
                Member {
                    address: ANNA,
                    name: "Anna".into(),
                    flags: flags[2],
                },
            ],
        }),
        ..Lev::default()
    };
    arrive(&mut model, 1, FAMILIE, 2, "", list([2 | 4, 4, 0]));
    arrive(&mut model, 2, FAMILIE, 3, "Lew: Brot ist da", copy(LEW));
    arrive(&mut model, 3, FAMILIE, 4, "Anna: Danke!", copy(ANNA));
    arrive(
        &mut model,
        4,
        LEW,
        5,
        "Lew invited you to Verein: lxma://…",
        invite(VEREIN, "Verein"),
    );
    update(&mut model, UiEvent::Clock(at(10)));
    let position = |model: &UiModel<Line>, peer: [u8; 16]| {
        model
            .conversations
            .iter()
            .position(|c| c.peer == peer)
            .expect("listed")
    };

    model.selected = NONE_OPEN;
    check("scene-group-list.json", &model.scene(LayoutClass::Compact));
    model.selected = position(&model, FAMILIE);
    check("scene-group-chat.json", &model.scene(LayoutClass::Compact));
    model.selected = position(&model, LEW);
    check(
        "scene-group-invitation.json",
        &model.scene(LayoutClass::Compact),
    );
    model.selected = position(&model, VEREIN);
    check(
        "scene-group-invited-chat.json",
        &model.scene(LayoutClass::Compact),
    );
    model.selected = position(&model, FAMILIE);
    update(
        &mut model,
        UiEvent::Group(Verb::Members {
            group: Target::Open,
        }),
    );
    check("scene-group-members.json", &model.scene(LayoutClass::Wide));
    update(
        &mut model,
        UiEvent::Group(Verb::Leave {
            group: Target::Open,
            confirmed: false,
        }),
    );
    check(
        "scene-group-members-leave.json",
        &model.scene(LayoutClass::Compact),
    );
    update(&mut model, UiEvent::Group(Verb::Close));
    update(&mut model, UiEvent::Group(Verb::Close));
    update(&mut model, UiEvent::Group(Verb::New));
    update(&mut model, UiEvent::Group(Verb::Pick(BOB)));
    check(
        "scene-group-new-pick.json",
        &model.scene(LayoutClass::Compact),
    );
    update(&mut model, UiEvent::Group(Verb::Next));
    update(&mut model, UiEvent::Group(Verb::Name("Wanderung".into())));
    check(
        "scene-group-new-name.json",
        &model.scene(LayoutClass::Compact),
    );
}
