//! The Rooms tab through the facade (1384): requests in the bytes the
//! app's Java writes, the model's fold, and the answer Java reads, with no
//! JNI and no network. What the hub says arrives as the hub link hands it
//! in (`loquax::rooms_link`'s events), so the phone's crossing is the one
//! under test; `rooms_hub.rs` runs the link itself.
//!
//! The golden `golden/rooms-room.json` is the answer of an open room as
//! Java decodes it, in German; `LOQUAX_WRITE_GOLDEN=1` writes it again.

use std::path::PathBuf;

use leviculum_rrc::LineKind;
use leviculum_ui::strings::Lang;
use loquax::ui::rooms::{RoomAsk, RoomsEvent};
use loquax::ui::scene::{LayoutClass, TabId};
use loquax::ui::{update, UiEffect, UiEvent, UiModel};
use loquax_android::rooms;
use loquax_android::schema::{self, Request, RoomsScreen};
use loquax_android::session::{self, Decoded, Platform};

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

const HUB: [u8; 16] = [0x07; 16];

/// A phone's model, with the hub named as the session names it from the
/// config, or none.
fn phone(hub: Option<[u8; 16]>) -> (tempfile::TempDir, UiModel<session::Line>) {
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    model.rooms.hub = hub;
    model.rooms.enabled = hub.is_some();
    model.lang = Lang::De;
    (home, model)
}

/// What Java's `Core.rooms` sends, decoded as the session decodes it,
/// folded.
fn tap(model: &mut UiModel<session::Line>, verb: &str, text: &str, index: i64) -> Vec<UiEffect> {
    let request: Request = schema::decode(&java_map(&[
        ("t", Java::Str("rooms")),
        ("verb", Java::Str(verb)),
        ("text", Java::Str(text)),
        ("index", Java::Long(index)),
    ]))
    .expect("a request");
    match session::decode(request).expect("a rooms verb") {
        Decoded::Platform(Platform::Rooms(events)) => events
            .into_iter()
            .flat_map(|event| update(model, event))
            .collect(),
        _ => panic!("a rooms verb is the platform's"),
    }
}

fn hub(model: &mut UiModel<session::Line>, event: RoomsEvent) {
    update(model, UiEvent::Rooms(event));
}

/// What Java reads back for the tab.
fn screen(model: &UiModel<session::Line>) -> RoomsScreen {
    schema::decode(&rooms::encode(model, Lang::De, 1)).expect("the answer")
}

fn line(nick: &str, kind: LineKind, text: &str, ours: bool) -> RoomsEvent {
    RoomsEvent::Line {
        room: "#lab".to_string(),
        kind,
        text: text.to_string(),
        nick: Some(nick.to_string()),
        ours,
        ts_ms: 1_800_000_000_000,
    }
}

#[test]
fn the_phone_works_a_room_through_the_schema() {
    let (_home, mut model) = phone(Some(HUB));
    assert!(!screen(&model).open, "closed until the tab is tapped");

    // The tab links (1652), once, and the first time the reader is told
    // their name is visible, which the config keeps (1577). "Verbinde…",
    // nothing to retry. A join meanwhile waits for the hub's welcome.
    let effects = tap(&mut model, "tab", "", 0);
    assert_eq!(
        effects,
        [
            UiEffect::RememberHash {
                key: "rooms_told",
                hash: Some(HUB)
            },
            UiEffect::Rooms(RoomAsk::Open { hub: HUB })
        ],
        "{effects:?}"
    );
    let linking = screen(&model);
    assert_eq!(linking.status, "Verbinde…");
    assert!(linking.retry.is_empty() && linking.listed.is_empty());
    assert!(
        tap(&mut model, "say", "#lab", 0).is_empty(),
        "the join waits"
    );
    assert!(tap(&mut model, "tab", "", 0).is_empty(), "linking already");
    // Before the welcome a line is not sent, and stays in the field.
    assert!(tap(&mut model, "say", "hello", 0).is_empty());
    assert_eq!(screen(&model).input, "hello");

    let effects = update(
        &mut model,
        UiEvent::Rooms(RoomsEvent::Welcomed {
            name: Some("periculum".to_string()),
            body_limit: 350,
        }),
    );
    assert_eq!(
        effects,
        [UiEffect::Rooms(RoomAsk::Join {
            room: "#lab".to_string(),
            key: None
        })],
        "the welcome sends the join that waited"
    );
    hub(
        &mut model,
        RoomsEvent::Joined {
            room: "#lab".to_string(),
            nick: None,
        },
    );
    hub(
        &mut model,
        RoomsEvent::Joined {
            room: "#lab".to_string(),
            nick: Some("marcel".to_string()),
        },
    );
    hub(
        &mut model,
        line("marcel", LineKind::Msg, "Moin aus Kiel", false),
    );
    // The room is listed as public, in the phone's language (1577).
    assert_eq!(screen(&model).rows[0].public, "öffentlich");
    let effects = tap(&mut model, "say", "/me winkt", 0);
    assert_eq!(
        effects,
        [UiEffect::Rooms(RoomAsk::Say {
            room: "#lab".to_string(),
            kind: LineKind::Action,
            text: "winkt".to_string()
        })]
    );
    hub(&mut model, line("lew", LineKind::Action, "winkt", true));
    hub(
        &mut model,
        RoomsEvent::HubNotice {
            room: Some("#lab".to_string()),
            text: "topic: the lab".to_string(),
        },
    );
    // Over the hub's limit: nothing is sent, the field keeps it.
    let long = "y".repeat(351);
    assert!(tap(&mut model, "say", &long, 0).is_empty());
    let shown = screen(&model);
    assert_eq!(shown.input, long);
    tap(&mut model, "say", "", 0);
    check("rooms-room.json", &screen(&model));

    // 1450: a room is a chat. The people under its name, their list
    // behind the header, and "Leave room" in its menu, which parts it as
    // `/part` does.
    let shown = screen(&model);
    assert_eq!(shown.subtitle, "marcel, du");
    assert!(shown.status.is_empty(), "a link that is up says nothing");
    assert_eq!(shown.placeholder, "Nachricht");
    let named: Vec<&str> = shown
        .lines
        .iter()
        .filter(|l| l.named)
        .map(|l| l.who.as_str())
        .collect();
    assert_eq!(named, ["marcel"], "another's run named once");
    tap(&mut model, "people", "", 1);
    let shown = screen(&model);
    assert!(shown.people_shown);
    assert_eq!(shown.people, ["marcel", "du"]);
    tap(&mut model, "people", "", 0);
    assert!(!screen(&model).people_shown);
    let leave = shown
        .menu
        .iter()
        .find(|m| m.verb == "leave")
        .expect("Leave room in the menu");
    assert_eq!(leave.label, "Raum verlassen");
    let effects = tap(&mut model, &leave.verb, "", leave.index);
    assert_eq!(
        effects,
        [UiEffect::Rooms(RoomAsk::Part {
            room: "#lab".to_string()
        })]
    );

    // The list, then the room again, then the chats.
    tap(&mut model, "show", "", -1);
    assert!(screen(&model).room.is_empty());
    tap(&mut model, "show", "", 0);
    assert_eq!(screen(&model).room, "#lab");
    tap(&mut model, "chats", "", 0);
    assert!(!screen(&model).open);
}

/// The control: a phone whose config names no hub has no tab, and its
/// answer says only that.
#[test]
fn without_a_hub_there_is_no_tab() {
    let (_home, mut model) = phone(None);
    assert_eq!(
        screen(&model),
        RoomsScreen {
            v: 1,
            ..RoomsScreen::default()
        }
    );
    assert!(tap(&mut model, "tab", "", 0).is_empty(), "nothing links");
    let tabs = model.scene(LayoutClass::Compact).tabs.expect("the tab bar");
    assert!(tabs.items.iter().all(|tab| tab.id != TabId::Rooms));

    // The rooms are Explore's (1578): with a hub or without, the bar is
    // the same three buttons.
    let (_home, model) = phone(Some(HUB));
    let tabs = model.scene(LayoutClass::Compact).tabs.expect("the tab bar");
    assert!(tabs.items.iter().all(|tab| tab.id != TabId::Rooms));
    assert!(tabs.items.iter().any(|tab| tab.id == TabId::Explore));
}

#[test]
fn a_verb_the_tab_does_not_know_is_an_error_for_java() {
    let request: Request = schema::decode(&java_map(&[
        ("t", Java::Str("rooms")),
        ("verb", Java::Str("kick")),
    ]))
    .expect("a request");
    assert!(session::decode(request).is_err());
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// The answer as Java reads it against its golden.
fn check(name: &str, shown: &RoomsScreen) {
    let mut got = serde_json::to_string_pretty(shown).expect("json");
    got.push('\n');
    let path = golden(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{name} moved; the answer now:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

/// Walk 11, finding 61 (1667): the chosen server's rooms screen while the
/// link is still coming lists the rooms already known of it (here the
/// mailbox's question named them), not "Connecting…" alone.
#[test]
fn the_known_rooms_stand_while_the_link_comes() {
    let (_home, mut model) = phone(Some(HUB));
    model
        .rooms
        .mailbox_named
        .insert(HUB, vec![String::from("general")]);
    tap(&mut model, "tab", "", 0);
    hub(&mut model, RoomsEvent::Linking);
    let shown = screen(&model);
    eprintln!(
        "FACADE linking: status={:?} listed={:?} title={:?}",
        shown.status, shown.listed, shown.listed_title
    );
    assert!(shown.status.contains("Verbinde"), "{:?}", shown.status);
    assert_eq!(shown.listed, vec![String::from("general")]);
}
