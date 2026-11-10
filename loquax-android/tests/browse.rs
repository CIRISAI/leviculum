//! The Browse tab through the facade (1337): requests in the bytes the
//! app's Java writes, the model's fold, and the answer Java reads, with no
//! JNI and no network. The page arrives as the fetch will hand it in (its
//! micron), so the phone's crossing is the one under test, not the fetch.

use leviculum_ui::strings::Lang;
use lnomad::DiscoveredNode;
use loquax::ui::browse::BrowseEvent;
use loquax::ui::scene::TabId;
use loquax::ui::{update, UiEffect, UiEvent, UiModel};
use loquax_android::browser;
use loquax_android::schema::{self, BrowseScreen, Request};
use loquax_android::session::{self, Decoded, Platform};

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

const NODE: [u8; 16] = [0x5a; 16];
const OPERATOR: [u8; 16] = [0x0e; 16];

fn hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A phone's model: the tab enabled as the session enables it.
fn phone() -> (tempfile::TempDir, UiModel<session::Line>) {
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    model.browse.enabled = true;
    model.lang = Lang::De;
    (home, model)
}

/// What Java sends for `verb`, decoded as the session decodes it, folded.
fn tap(model: &mut UiModel<session::Line>, entries: &[(&str, Java)]) -> Vec<UiEffect> {
    let mut map = vec![("t", Java::Str("browse"))];
    map.extend(entries.iter().map(|(k, v)| (*k, copy(v))));
    let request: Request = schema::decode(&java_map(&map)).expect("a request");
    match session::decode(request).expect("a browse verb") {
        Decoded::Platform(Platform::Browse(event)) => update(model, event),
        _ => panic!("a browse verb is the platform's"),
    }
}

fn copy<'a>(value: &Java<'a>) -> Java<'a> {
    match value {
        Java::Str(s) => Java::Str(s),
        Java::Long(n) => Java::Long(*n),
        _ => unreachable!("the browse requests carry strings and numbers"),
    }
}

/// What Java reads back for the tab.
fn screen(model: &UiModel<session::Line>) -> BrowseScreen {
    schema::decode(&browser::encode(model, Lang::De, 1)).expect("the answer")
}

#[test]
fn the_phone_reads_a_page_through_the_scene() {
    let (_home, mut model) = phone();
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Node(DiscoveredNode {
            dest_hash: NODE,
            name: Some("Marcels Knoten".to_string()),
            first_seen: 1_800_000_000,
            last_seen: 1_800_000_000,
            hops: Some(2),
        })),
    );
    assert!(!screen(&model).open, "closed until the tab is tapped");

    tap(&mut model, &[("verb", Java::Str("tab"))]);
    tap(
        &mut model,
        &[
            ("verb", Java::Str("size")),
            ("cols", Java::Long(40)),
            ("rows", Java::Long(20)),
        ],
    );
    let list = screen(&model);
    assert!(list.open);
    let node = list
        .rows
        .iter()
        .find(|row| row.kind == "item" && row.text.contains("Marcels Knoten"))
        .unwrap_or_else(|| panic!("the node is a row: {:?}", list.rows));
    assert!(node.node.starts_with("place_"), "{node:?}");
    assert!(
        list.rows.iter().any(|row| row.kind == "heading"),
        "the sections are headed"
    );

    // A tap on the row asks the platform for the node's page.
    let effects = tap(
        &mut model,
        &[
            ("verb", Java::Str("activate")),
            ("node", Java::Str(&node.node)),
        ],
    );
    assert!(
        matches!(&effects[..], [UiEffect::Browse(lnomad::Effect::Navigate(t))]
            if t.dest_hash == NODE && t.path == "/page/index.mu"),
        "{effects:?}"
    );
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Loaded {
            micron: format!(
                ">Willkommen\n\n`[Weiter`:/page/next.mu]\n`[Schreib mir`lxmf@{}]\n\nName: `<name`>",
                hex(&OPERATOR)
            ),
            title: "Marcels Knoten".to_string(),
        }),
    );
    let page = screen(&model);
    assert_eq!(page.title, "Marcels Knoten");
    assert!(page.rows.is_empty(), "the page replaced the list");
    let runs: Vec<_> = page.lines.iter().flat_map(|l| l.runs.iter()).collect();
    assert!(runs.iter().any(|r| r.text.contains("Willkommen")));
    let next = runs
        .iter()
        .find(|r| r.text.contains("Weiter"))
        .expect("the link's run");
    assert_eq!(next.node, "link_1");
    assert!(
        next.underline && !next.fg.is_empty(),
        "a link looks like one"
    );
    assert!(runs.iter().any(|r| r.node == "field_1"), "the field's run");
    assert!(page.lines.len() <= 20, "a window, never the whole page");

    // Java's text field fills the form; the run shows it.
    tap(
        &mut model,
        &[
            ("verb", Java::Str("fill")),
            ("node", Java::Str("field_1")),
            ("text", Java::Str("Lew")),
        ],
    );
    let filled = screen(&model);
    assert!(filled
        .lines
        .iter()
        .flat_map(|l| l.runs.iter())
        .any(|r| r.node == "field_1" && r.text.starts_with("Lew")));

    // The link follows; back is offered and comes from the cache.
    let effects = tap(
        &mut model,
        &[
            ("verb", Java::Str("activate")),
            ("node", Java::Str("link_1")),
        ],
    );
    assert!(
        matches!(&effects[..], [UiEffect::Browse(lnomad::Effect::Navigate(t))] if t.path == "/page/next.mu")
    );
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Loaded {
            micron: "Seite zwei".to_string(),
            title: "Marcels Knoten".to_string(),
        }),
    );
    assert!(screen(&model).back);
    tap(&mut model, &[("verb", Java::Str("back"))]);
    assert!(screen(&model)
        .lines
        .iter()
        .flat_map(|l| l.runs.iter())
        .any(|r| r.text.contains("Willkommen")));

    // The operator's link opens the chat, and the tab gives way.
    let write = screen(&model)
        .lines
        .iter()
        .flat_map(|l| l.runs.clone())
        .find(|r| r.text.contains("Schreib mir"))
        .expect("the operator's link");
    assert_eq!(
        tap(
            &mut model,
            &[
                ("verb", Java::Str("activate")),
                ("node", Java::Str(&write.node))
            ],
        ),
        []
    );
    assert!(!screen(&model).open);
    assert_eq!(model.conversations[model.selected].peer, OPERATOR);
    tap(&mut model, &[("verb", Java::Str("tab"))]);
    assert!(screen(&model).open, "and the tab comes back where it was");
    tap(&mut model, &[("verb", Java::Str("chats"))]);
    assert!(!screen(&model).open);
}

#[test]
fn the_tab_is_in_the_phones_tab_bar() {
    let (_home, model) = phone();
    let tabs = model
        .scene(loquax::ui::scene::LayoutClass::Compact)
        .tabs
        .expect("the tab bar");
    // The pages are Explore's (1578): three buttons, no fourth.
    let ids: Vec<TabId> = tabs.items.iter().map(|tab| tab.id).collect();
    assert_eq!(ids, [TabId::Chats, TabId::Map, TabId::Explore]);
}

#[test]
fn a_verb_the_tab_does_not_know_is_an_error_for_java() {
    let request: Request = schema::decode(&java_map(&[
        ("t", Java::Str("browse")),
        ("verb", Java::Str("jump")),
    ]))
    .expect("a request");
    assert!(session::decode(request).is_err());
}
