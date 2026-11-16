//! The Explore tab through the facade (1618, the walk of 17024c3a on the
//! Pixel): requests in the bytes the app's Java writes, the model's fold,
//! and the answer Java reads, with no JNI and no network.
//!
//! What the Java does with the answer (the field that never loses a
//! character, the list that keeps a row under a finger) is
//! `ExploreRows.java`, run on the host by `fling.rs`; this is the
//! library's half: the order it answers while a finger is down, the
//! words it says, and a server's sheet that changes nothing.

use leviculum_ui::strings::Lang;
use lnomad::DiscoveredNode;
use loquax::ui::browse::BrowseEvent;
use loquax::ui::{update, UiEffect, UiEvent, UiModel};
use loquax_android::explore;
use loquax_android::schema::{self, ExploreScreen, Request};
use loquax_android::session::{self, Decoded, Platform};

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

/// 2027-01-15 12:00 UTC.
const NOW: i64 = 1_800_000_000;

/// A phone's model with the browser on, as the session sets it from a
/// config without `browse_tab` (on since 1618), three pages heard.
fn phone() -> (tempfile::TempDir, UiModel<session::Line>) {
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    let config = loquax::config::load(home.path()).expect("no config");
    model.browse.enabled = config.browse_on();
    model.lang = Lang::De;
    update(
        &mut model,
        UiEvent::Clock(loquax::when::WallTime {
            unix_secs: NOW,
            offset_secs: 0,
        }),
    );
    update(&mut model, UiEvent::Elapsed(10_000));
    for (byte, name, ago) in [
        (0x11, Some("Radio RPi4"), 600),
        (0x22, Some("IranTalk"), 3_600),
        (0x33, None, 7_200),
    ] {
        heard(&mut model, byte, name, ago);
    }
    (home, model)
}

fn heard(model: &mut UiModel<session::Line>, byte: u8, name: Option<&str>, ago: i64) {
    let at = u64::try_from(NOW - ago).expect("positive");
    update(
        model,
        UiEvent::Browse(BrowseEvent::Node(DiscoveredNode {
            dest_hash: [byte; 16],
            name: name.map(ToString::to_string),
            first_seen: at,
            last_seen: at,
            hops: Some(5),
        })),
    );
}

/// What Java's `Core.explore` sends, decoded as the session decodes it,
/// folded.
fn tap(
    model: &mut UiModel<session::Line>,
    verb: &str,
    text: &str,
    dest: &str,
    index: i64,
) -> Vec<UiEffect> {
    let request: Request = schema::decode(&java_map(&[
        ("t", Java::Str("explore")),
        ("verb", Java::Str(verb)),
        ("text", Java::Str(text)),
        ("dest", Java::Str(dest)),
        ("index", Java::Long(index)),
    ]))
    .expect("a request");
    match session::decode(request).expect("an explore verb") {
        Decoded::Platform(Platform::Rooms(events)) => events
            .into_iter()
            .flat_map(|event| update(model, event))
            .collect(),
        _ => panic!("an explore verb is the platform's"),
    }
}

/// What Java reads back for the tab.
fn screen(model: &UiModel<session::Line>) -> ExploreScreen {
    schema::decode(&explore::encode(model, Lang::De, 1)).expect("the answer")
}

fn labels(shown: &ExploreScreen) -> Vec<String> {
    shown.pages.iter().map(|p| p.label.clone()).collect()
}

/// Walk 4: a page opens. The browser is on where the config does not
/// say off, so the banner is gone and a tap fetches the page.
#[test]
fn a_page_opens_and_no_banner_says_it_cannot() {
    let (_home, mut model) = phone();
    assert!(model.browse.enabled, "on unless the config says off");
    tap(&mut model, "tab", "", "", 0);
    let shown = screen(&model);
    assert_eq!(shown.pages_off, "");
    let effects = tap(&mut model, "open", "", &"11".repeat(16), 0);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, UiEffect::Browse(lnomad::Effect::Navigate(_)))),
        "{effects:?}"
    );
    assert_eq!(screen(&model).showing, "browse");
}

/// Walk 5 and 13: a row is its name alone, a nameless page "Unbenannte
/// Seite", and the line under it one wording of "gesehen" and how far in
/// stations, never "Hops".
#[test]
fn a_row_is_a_name_and_its_line_says_seen_and_how_far() {
    let (_home, mut model) = phone();
    tap(&mut model, "tab", "", "", 0);
    let shown = screen(&model);
    assert_eq!(
        labels(&shown),
        ["Radio RPi4", "IranTalk", "Unbenannte Seite"]
    );
    assert_eq!(
        shown.pages[0].said,
        "gesehen vor 10 Min. · über 5 Stationen erreichbar"
    );
    assert_eq!(shown.placeholder, "Suchen");
    assert!(labels(&shown).iter().all(|l| !l.contains('·')), "no hash");
    // The library's own words; a server's own name may say what it likes.
    for text in shown
        .pages
        .iter()
        .map(|p| p.said.clone())
        .chain(shown.hubs.iter().map(|h| h.value.clone()))
        .chain([
            shown.servers_title.clone(),
            shown.rooms_title.clone(),
            shown.joined_empty.clone(),
            shown.servers_empty.clone(),
        ])
    {
        assert!(!text.contains("  "), "no double space: {text:?}");
        let lower = text.to_lowercase();
        assert!(!lower.contains("hop") && !lower.contains("hub"), "{text}");
    }
}

/// Walk 1: 32 characters typed fast, each one a `typed` verb; the
/// library ends with all 32, and each answer in between holds the text of
/// its verb, older than the field: the echo Java no longer writes back
/// (`ExploreRows.java`, `echo`).
#[test]
fn thirty_two_fast_characters_all_arrive() {
    let (_home, mut model) = phone();
    tap(&mut model, "tab", "", "", 0);
    let typed = "0123456789abcdef0123456789abcdef";
    let mut answers = Vec::new();
    for end in 1..=typed.len() {
        tap(&mut model, "typed", &typed[..end], "", 0);
        answers.push(screen(&model).query);
    }
    assert_eq!(screen(&model).query, typed);
    assert_eq!(answers.len(), 32);
    assert!(answers.iter().zip(1..).all(|(q, n)| q.len() == n));
}

/// Walk 3: a tap during a re-sort hits the row that was under the finger.
/// The finger comes down on the third row; a page is heard that would
/// stand first; every answer while the finger is down keeps the order,
/// and the tap on the third row opens the page that was there. The new
/// order comes after the lift.
#[test]
fn a_tap_during_a_resort_hits_the_row_under_the_finger() {
    let (_home, mut model) = phone();
    tap(&mut model, "tab", "", "", 0);
    let before = screen(&model);
    let under = before.pages[2].dest.clone();
    tap(&mut model, "touch", "", "", 1);
    heard(&mut model, 0x33, Some("Neu gehört"), 0);
    update(&mut model, UiEvent::Elapsed(12_000));
    let during = screen(&model);
    let dests = |s: &ExploreScreen| s.pages.iter().map(|p| p.dest.clone()).collect::<Vec<_>>();
    assert_eq!(
        dests(&during),
        dests(&before),
        "nothing moved under the finger"
    );
    tap(&mut model, "touch", "", "", 0);
    let effects = tap(&mut model, "open", "", &under, 2);
    let opened = effects.iter().find_map(|e| match e {
        UiEffect::Browse(lnomad::Effect::Navigate(target)) => Some(target.dest_hash),
        _ => None,
    });
    assert_eq!(
        opened,
        Some([0x33; 16]),
        "the row that was under the finger"
    );
    let after = screen(&model);
    assert_eq!(
        after.pages[0].dest, under,
        "the new order came after the lift"
    );
    // The control: without the hold the heard page moves first at once.
    let (_home2, mut free) = phone();
    tap(&mut free, "tab", "", "", 0);
    heard(&mut free, 0x33, Some("Neu gehört"), 0);
    update(&mut free, UiEvent::Elapsed(12_000));
    assert_eq!(screen(&free).pages[0].dest, "33".repeat(16));
}

/// Walk 7: a search finds names and a pasted address, never a hash's
/// letters, and filters the servers too.
#[test]
fn the_search_finds_names_and_addresses_only() {
    let (_home, mut model) = phone();
    tap(&mut model, "tab", "", "", 0);
    tap(&mut model, "typed", "11", "", 0);
    assert!(
        labels(&screen(&model)).is_empty(),
        "a hash's letters find nothing"
    );
    tap(&mut model, "typed", "radio", "", 0);
    assert_eq!(labels(&screen(&model)), ["Radio RPi4"]);
    tap(&mut model, "typed", &"22".repeat(16), "", 0);
    assert_eq!(labels(&screen(&model)), ["IranTalk"]);
    tap(&mut model, "typed", "zzzz-no-server", "", 0);
    let shown = screen(&model);
    assert!(shown.hubs.is_empty(), "{:?}", shown.hubs);
    assert_eq!(
        shown.servers_empty,
        "Kein Server passt zu „zzzz-no-server“."
    );
}

/// Walk 2, new 4 to 6: the rooms' section says what it holds: "Räume"
/// with the rooms the reader is in, or one sentence; then "Server" with
/// every server, each saying when it was seen.
#[test]
fn the_rooms_section_says_what_it_holds() {
    let (_home, mut model) = phone();
    tap(&mut model, "tab", "", "", 0);
    let shown = screen(&model);
    assert_eq!(shown.rooms_title, "Räume");
    assert!(shown.joined.is_empty());
    assert_eq!(
        shown.joined_empty,
        "Du bist in keinem Raum. Such dir unten einen Server aus."
    );
    assert_eq!(shown.servers_title, "Server");
    for hub in &shown.hubs {
        assert!(
            hub.value.starts_with("noch nicht gesehen") || hub.value.starts_with("gesehen"),
            "a seen line on every server: {:?}",
            hub.value
        );
    }
    assert!(
        shown
            .hubs
            .iter()
            .filter(|h| h.value.contains("empfohlen"))
            .count()
            <= 1,
        "one suggested at most"
    );
}

/// Walk 10, 6: a tap on a server opens its sheet and changes nothing; the
/// sheet names it, says how far in stations, and its button makes it the
/// public rooms' server.
#[test]
fn a_server_opens_its_sheet_and_only_the_button_chooses_it() {
    let (_home, mut model) = phone();
    tap(&mut model, "tab", "", "", 0);
    let shown = screen(&model);
    let Some(first) = shown.hubs.first().cloned() else {
        return;
    };
    assert!(!first.name.trim().is_empty(), "no nameless row");
    let effects = tap(&mut model, "hub", "", &first.dest, 0);
    // It asks the server for its rooms, on a question of its own (1621),
    // and nothing else.
    assert!(
        effects
            .iter()
            .all(|e| matches!(e, UiEffect::Rooms(loquax::ui::rooms::RoomAsk::Peek { .. }))),
        "{effects:?}"
    );
    assert_eq!(model.rooms.hub, None, "the tap chose nothing");
    let sheet = screen(&model);
    assert_eq!(sheet.sheet_dest, first.dest);
    assert_eq!(sheet.sheet_name, first.name);
    assert_eq!(sheet.sheet_button, "Diesen Server für Räume nutzen");
    if sheet.sheet_rooms.is_empty() {
        assert_eq!(
            sheet.sheet_rooms_note,
            "Frage den Server nach seinen Räumen…"
        );
    }
    // A shipped server this phone never heard: when, and no distance
    // measured elsewhere (1629, walk 3 new 5).
    assert_eq!(sheet.sheet_said, "noch nicht gesehen");
    tap(&mut model, "close", "", "", 0);
    assert_eq!(screen(&model).sheet_dest, "");
    tap(&mut model, "hub", "", &first.dest, 0);
    let effects = tap(&mut model, "use", "", &first.dest, 0);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            UiEffect::RememberHash {
                key: "rooms_hub",
                ..
            }
        )),
        "{effects:?}"
    );
    assert_eq!(screen(&model).showing, "rooms");
}

/// What Java's `Core.browse` sends for `verb`, folded.
fn browse_verb(model: &mut UiModel<session::Line>, verb: &str, text: &str) -> Vec<UiEffect> {
    let request: Request = schema::decode(&java_map(&[
        ("t", Java::Str("browse")),
        ("verb", Java::Str(verb)),
        ("node", Java::Str("")),
        ("text", Java::Str(text)),
    ]))
    .expect("a request");
    match session::decode(request).expect("a browse verb") {
        Decoded::Platform(Platform::Browse(event)) => update(model, event),
        _ => panic!("a browse verb is the platform's"),
    }
}

/// What the Browse pane reads back, said for the report as the dump a
/// device walk would show.
fn page(model: &UiModel<session::Line>, when: &str) -> loquax_android::schema::BrowseScreen {
    let shown: loquax_android::schema::BrowseScreen =
        schema::decode(&loquax_android::browser::encode(model, Lang::De, 1)).expect("the answer");
    let lines: Vec<String> = shown
        .lines
        .iter()
        .map(|l| l.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .filter(|l| !l.trim().is_empty())
        .collect();
    println!(
        "{when}: title={:?} address={:?} status={:?} loading={} loading_words={:?} sheet={:?} rows={} lines={lines:?}",
        shown.title,
        shown.address,
        shown.status,
        shown.loading,
        shown.loading_words,
        shown.sheet,
        shown.rows.len()
    );
    shown
}

/// Walk 3 new 1 (1629): a page opened from Explore loads. The phone
/// says, in this order, Explore's `open`, then (its next screen saying
/// "browse") the Browse pane's `tab`; the page arrives from the fetch.
/// Before the fix the `tab` drew the start screen through the load path,
/// which took the fetch in flight: no loading line, the places list
/// under the page's name, and the page never came. "Los" on the unchanged
/// address waits while the page is on its way and loads it again after.
#[test]
fn a_page_opened_from_explore_shows_its_loading_line_then_the_page() {
    let (_home, mut model) = phone();
    let asked = u64::try_from(NOW).expect("positive");
    model.browse.model.clock = Some(asked);
    tap(&mut model, "tab", "", "", 0);
    tap(&mut model, "open", "", &"22".repeat(16), 0);
    assert_eq!(screen(&model).showing, "browse");
    browse_verb(&mut model, "tab", "");
    let loading = page(&model, "after open and tab");
    assert_eq!(loading.title, "IranTalk");
    assert_eq!(loading.status, "Lade IranTalk");
    assert!(loading.loading);
    // 1632, walk 4 new 12: the loading state in the middle, the page's
    // name in it, the seconds once three have passed.
    assert_eq!(loading.loading_words, "Lade IranTalk …");
    model.browse.model.clock = Some(asked + 2);
    assert_eq!(page(&model, "2 s later").loading_words, "Lade IranTalk …");
    model.browse.model.clock = Some(asked + 7);
    assert_eq!(
        page(&model, "7 s later").loading_words,
        "Lade IranTalk … 7 s"
    );
    assert_eq!(loading.sheet, "", "no places list over the page");
    assert!(loading.rows.is_empty());
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Loaded {
            micron: "Willkommen bei IranTalk".into(),
            title: "IranTalk".into(),
        }),
    );
    let shown = page(&model, "after the page came");
    assert_eq!(shown.status, "", "the loading line leaves with the page");
    assert!(!shown.loading);
    assert_eq!(shown.loading_words, "", "and the loading state with it");
    assert_eq!(model.browse.loading_since, None);
    let text: String = shown
        .lines
        .iter()
        .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
        .collect();
    assert!(text.contains("Willkommen bei IranTalk"), "{text}");
    // "Los" on the unchanged address, nothing on its way: the page again.
    let effects = browse_verb(&mut model, "reload", "");
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, UiEffect::Browse(lnomad::Effect::Navigate(_)))),
        "{effects:?}"
    );
    let again = page(&model, "after Los");
    assert!(again.loading);
    // The page stands while it comes again: the status line says so, no
    // loading state over it.
    assert_eq!(again.loading_words, "");
    assert_eq!(again.status, "Lade IranTalk");
}

/// The phone's model on `home` as the session starts it: the store, the
/// config, and what was heard before ([`loquax::heard::load`]).
fn started(home: &std::path::Path, me: [u8; 16]) -> (loquax::store::Store, UiModel<session::Line>) {
    let store = loquax::store::Store::open_in(home, me).expect("the store");
    let mut model = session::known(home, &store).expect("a model");
    let config = loquax::config::load(home).expect("the config");
    model.browse.enabled = config.browse_on();
    model.lang = Lang::De;
    update(
        &mut model,
        UiEvent::Clock(loquax::when::WallTime {
            unix_secs: NOW,
            offset_secs: 0,
        }),
    );
    let (pages, hubs) = loquax::heard::load(&store).expect("the heard rows");
    loquax::ui::explore::restore_heard(&mut model, pages, hubs, config.rooms_suggested);
    (store, model)
}

fn hub_heard(model: &mut UiModel<session::Line>, hub: &loquax::ui::hubs::Hub, hops: u8, ago: i64) {
    update(
        model,
        UiEvent::Engine(loquax::outbox::OutboxEvent::HubAnnounced {
            hub: loquax::ui::hubs::HeardHub {
                hash: hub.hash,
                name: Some(hub.name.clone()),
                hops: Some(hops),
                first: NOW - ago,
                last: NOW - ago,
                answered: None,
            },
        }),
    );
}

/// The server rows' names and the one "empfohlen" marks.
fn servers(shown: &ExploreScreen) -> (Vec<String>, Option<String>) {
    (
        shown.hubs.iter().map(|h| h.name.clone()).collect(),
        shown
            .hubs
            .iter()
            .find(|h| h.value.contains("empfohlen"))
            .map(|h| h.name.clone()),
    )
}

/// Walk 4 of 1632, new 15: after a restart Explore listed one to four
/// pages of over twenty, and "empfohlen" sat on another server. What the
/// phone heard is kept in its store as the session's persist keeps it;
/// a core started again on the same store lists the same pages, each with
/// its old "gesehen vor", and marks the same server. The control: the
/// same hearing never kept starts empty, and without the held suggestion
/// the closer server heard later takes the mark.
#[test]
fn a_restart_lists_what_was_heard_and_holds_the_suggestion() {
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let shipped = loquax::ui::hubs::shipped();
    let (far, near) = (&shipped[0], &shipped[1]);
    let before = {
        let (mut store, mut model) = started(home.path(), me);
        for (byte, name, ago) in [
            (0x11, Some("Radio RPi4"), 600),
            (0x22, Some("IranTalk"), 3_600),
            (0x33, None, 7_200),
            (0x44, Some("Ben's Library"), 90_000),
        ] {
            heard(&mut model, byte, name, ago);
        }
        // The far server is heard first and suggested; a closer one heard
        // after does not take the mark (1629).
        hub_heard(&mut model, far, 6, 300);
        hub_heard(&mut model, near, 1, 60);
        tap(&mut model, "tab", "", "", 0);
        let shown = screen(&model);
        assert_eq!(servers(&shown).1.as_deref(), Some(far.name.as_str()));
        let owed = loquax::ui::explore::take_heard(&mut model);
        assert_eq!((owed.pages.len(), owed.hubs.len()), (4, 2), "{owed:?}");
        loquax::heard::keep(&mut store, home.path(), owed).expect("kept");
        shown
    };
    assert_eq!(labels(&before).len(), 4, "{:?}", labels(&before));
    let (_store, mut model) = started(home.path(), me);
    tap(&mut model, "tab", "", "", 0);
    let after = screen(&model);
    assert_eq!(after.pages, before.pages, "the same rows, the same words");
    assert_eq!(servers(&after), servers(&before));
    assert_eq!(after.hubs, before.hubs);
    // Nothing of it is owed again: the restart wrote nothing.
    assert_eq!(
        loquax::ui::explore::take_heard(&mut model),
        loquax::ui::explore::HeardOwed::default()
    );
    // The controls. A store nobody kept into starts with nothing.
    let empty = tempfile::tempdir().expect("app dir");
    let (_, mut fresh) = started(empty.path(), me);
    tap(&mut fresh, "tab", "", "", 0);
    assert!(labels(&screen(&fresh)).is_empty());
    // The hubs kept without the held suggestion: the closer one takes it.
    loquax::config::save_hash(home.path(), loquax::heard::SUGGESTED_KEY, None).expect("unset");
    let (_, mut unheld) = started(home.path(), me);
    tap(&mut unheld, "tab", "", "", 0);
    assert_eq!(
        servers(&screen(&unheld)).1.as_deref(),
        Some(near.name.as_str())
    );
}

/// Walk 8 of 1653, 41: no server said "empfohlen", where walk 5 had it on
/// ValleiRug. A restart holds the suggestion it kept (1632); a held hub
/// the store no longer lists, or one not heard yet this session, must not
/// take the mark with it. Through the facade, on the same store: the held
/// hub never kept and another named shipped hub kept, the restart marks
/// the kept one; nothing kept at all, no mark, and the first named shipped
/// hub heard this session takes it, the held one too when it comes back.
/// The control: a hub in force marks none.
#[test]
fn a_restart_with_a_held_hub_not_heard_yet_still_suggests_one() {
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let shipped = loquax::ui::hubs::shipped();
    let (held, other) = (&shipped[0], &shipped[1]);
    // The held hub was never kept; another was.
    let home = tempfile::tempdir().expect("app dir");
    {
        let (mut store, mut model) = started(home.path(), me);
        hub_heard(&mut model, other, 3, 600);
        let owed = loquax::ui::explore::take_heard(&mut model);
        loquax::heard::keep(&mut store, home.path(), owed).expect("kept");
    }
    loquax::config::save_hash(home.path(), loquax::heard::SUGGESTED_KEY, Some(held.hash))
        .expect("held");
    let (_store, mut model) = started(home.path(), me);
    tap(&mut model, "tab", "", "", 0);
    assert_eq!(
        servers(&screen(&model)).1.as_deref(),
        Some(other.name.as_str()),
        "the kept hub takes the mark: {:?}",
        servers(&screen(&model))
    );
    // The held one heard again this session: the mark stays where it is.
    hub_heard(&mut model, held, 1, 10);
    assert_eq!(
        servers(&screen(&model)).1.as_deref(),
        Some(other.name.as_str())
    );
    // Nothing kept, the held hub in the config: no mark until a named
    // shipped hub is heard, then that one.
    let bare = tempfile::tempdir().expect("app dir");
    loquax::config::save_hash(bare.path(), loquax::heard::SUGGESTED_KEY, Some(held.hash))
        .expect("held");
    let (_store, mut cold) = started(bare.path(), me);
    tap(&mut cold, "tab", "", "", 0);
    assert_eq!(
        servers(&screen(&cold)).1,
        None,
        "nothing heard, nothing suggested"
    );
    hub_heard(&mut cold, other, 4, 30);
    assert_eq!(
        servers(&screen(&cold)).1.as_deref(),
        Some(other.name.as_str()),
        "the first heard takes it"
    );
    // The held hub first, after the restart: it is the one.
    let again = tempfile::tempdir().expect("app dir");
    loquax::config::save_hash(again.path(), loquax::heard::SUGGESTED_KEY, Some(held.hash))
        .expect("held");
    let (_store, mut back) = started(again.path(), me);
    hub_heard(&mut back, held, 6, 30);
    hub_heard(&mut back, other, 1, 20);
    tap(&mut back, "tab", "", "", 0);
    assert_eq!(
        servers(&screen(&back)).1.as_deref(),
        Some(held.name.as_str())
    );
    // The control: a hub in force, no mark on any.
    loquax::config::save_hash(again.path(), "rooms_hub", Some(other.hash)).expect("in force");
    let (_store, mut chosen) = started(again.path(), me);
    let config = loquax::config::load(again.path()).expect("config");
    chosen.rooms.hub = config.rooms_hub;
    hub_heard(&mut chosen, held, 6, 30);
    tap(&mut chosen, "tab", "", "", 0);
    assert_eq!(servers(&screen(&chosen)).1, None, "a hub in force: no mark");
}

/// Walk 4 of 1632, new 14: the browser's "Orte" sheet on the phone said
/// "Gefundene Knoten" and rows like "RNS Search · 3e5f19fd… · 110
/// Sprünge". Through the facade: the heading says what the rows are, a
/// row is a name with when it was heard and how far, no hash, and a hop
/// count no reader can use says nothing.
#[test]
fn the_places_sheet_names_pages_and_never_hashes() {
    let (_home, mut model) = phone();
    model.browse.model.clock = Some(u64::try_from(NOW).expect("positive"));
    heard(&mut model, 0x3e, Some("RNS Search"), 120);
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Node(DiscoveredNode {
            dest_hash: [0x3e; 16],
            name: Some("RNS Search".into()),
            first_seen: u64::try_from(NOW - 120).expect("positive"),
            last_seen: u64::try_from(NOW - 120).expect("positive"),
            hops: Some(110),
        })),
    );
    tap(&mut model, "tab", "", "", 0);
    tap(&mut model, "open", "", &"11".repeat(16), 0);
    browse_verb(&mut model, "tab", "");
    browse_verb(&mut model, "places", "");
    let shown = page(&model, "the places sheet");
    assert_eq!(shown.loading_words, "", "no loading state over the sheet");
    let rows: Vec<String> = shown.rows.iter().map(|r| r.text.clone()).collect();
    println!("places rows: {rows:#?}");
    assert!(rows.iter().any(|r| r == "Gehörte Seiten"), "{rows:?}");
    assert!(
        rows.iter()
            .any(|r| r == "RNS Search  ·  gesehen vor 2 Min."),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r == "Radio RPi4  ·  gesehen vor 10 Min.  ·  über 5 Stationen erreichbar"),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with("Unbenannte Seite  ·  ")),
        "{rows:?}"
    );
    for row in &rows {
        assert!(!row.contains("Sprünge") && !row.contains('…'), "{row}");
        assert!(!row.contains("3e3e") && !row.contains("1111"), "{row}");
    }
}

/// Walk 5 of 1639, new 26: scrolled, a server row said "vor 19 Min. ·
/// über 8 Stationen" while its sheet said "gerade eben · über 5
/// Stationen". The library reads both from one source
/// (`settings::offered_hubs`) at the same moment: after the server is
/// heard again, nearer, the row and the sheet say the same when and how
/// far. (The phone showed an older row because its list held a waiting
/// answer whole; `ExploreRows.refresh` is that half, in `fling.rs`.)
#[test]
fn a_server_row_and_its_sheet_say_the_same() {
    let (_home, mut model) = phone();
    let hub = loquax::ui::hubs::shipped()[0].clone();
    hub_heard(&mut model, &hub, 8, 19 * 60);
    tap(&mut model, "tab", "", "", 0);
    let dest: String = hub.hash.iter().map(|b| format!("{b:02x}")).collect();
    let row = |shown: &ExploreScreen| {
        shown
            .hubs
            .iter()
            .find(|h| h.dest == dest)
            .map(|h| h.value.clone())
            .expect("the server's row")
    };
    let before = row(&screen(&model));
    assert!(
        before.starts_with("gesehen vor 19 Min. · über 8 Stationen"),
        "{before}"
    );
    hub_heard(&mut model, &hub, 5, 0);
    tap(&mut model, "hub", "", &dest, 0);
    let shown = screen(&model);
    println!("row: {:?}  sheet: {:?}", row(&shown), shown.sheet_said);
    assert!(
        shown
            .sheet_said
            .starts_with("gesehen gerade eben · über 5 Stationen"),
        "{}",
        shown.sheet_said
    );
    assert!(
        row(&shown).starts_with(&shown.sheet_said),
        "row {:?}, sheet {:?}",
        row(&shown),
        shown.sheet_said
    );
}

/// Walk 5 of 1639, new 23: the places sheet and the page shared one
/// scroll on the phone; past the sheet's last entry the page followed, and
/// the sheet still counted as open while it was off screen, so the next
/// tap on "Orte" closed it unseen. Through the facade: with the sheet
/// open, a scroll past its end is the sheet's, the page under it stays at
/// its first line, and the sheet stays open until it is closed; "Orte"
/// again closes it and the page is where it was.
#[test]
fn the_places_sheet_scrolls_alone_and_stays_what_is_shown() {
    let (_home, mut model) = phone();
    model.browse.model.clock = Some(u64::try_from(NOW).expect("positive"));
    tap(&mut model, "tab", "", "", 0);
    tap(&mut model, "open", "", &"11".repeat(16), 0);
    browse_verb(&mut model, "tab", "");
    let long: String = (0..200).map(|n| format!("Zeile {n}\n")).collect();
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Loaded {
            micron: long,
            title: "Radio RPi4".into(),
        }),
    );
    let first = page(&model, "the page").first;
    browse_verb(&mut model, "places", "");
    let open = page(&model, "the sheet open");
    assert_eq!(open.sheet, "Orte");
    for _ in 0..30 {
        browse_verb(&mut model, "scroll", "pagedown");
    }
    let scrolled = page(&model, "thirty pages down");
    assert_eq!(scrolled.sheet, "Orte", "the sheet is still what is shown");
    let texts = |s: &loquax_android::schema::BrowseScreen| -> Vec<String> {
        s.rows.iter().map(|r| r.text.clone()).collect()
    };
    assert_eq!(texts(&scrolled), texts(&open), "the same rows");
    assert_eq!(scrolled.first, first, "the page under it did not move");
    browse_verb(&mut model, "places", "");
    let closed = page(&model, "Orte again");
    assert_eq!(closed.sheet, "", "the second tap closes it");
    assert_eq!(closed.first, first);
}

/// The phone's answer for the tab as Java decodes it, against
/// `golden/<name>`; `LOQUAX_WRITE_GOLDEN=1` writes it again.
fn check(name: &str, shown: &ExploreScreen) {
    let mut got = serde_json::to_string_pretty(shown).expect("json");
    got.push('\n');
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{name} moved; the answer now:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

fn clock(model: &mut UiModel<session::Line>, unix_secs: i64) {
    update(
        model,
        UiEvent::Clock(loquax::when::WallTime {
            unix_secs,
            offset_secs: 0,
        }),
    );
}

fn mailbox(model: &mut UiModel<session::Line>, up: bool) {
    update(
        model,
        UiEvent::Explore(loquax::ui::explore::ExploreEvent::Mailbox(up)),
    );
}

/// 1644: opening the tab through the facade asks the mailbox once, a
/// second open within the minute does not; the line on top of the answer
/// Java reads says how old the list is in its three states, and a page
/// silent for seven days is not in it while one at six days and 23 hours
/// is. Goldens: `explore-as-of-old.json` (up, three hours old) and
/// `explore-as-of-down.json` (down, as of yesterday 14:20).
#[test]
fn the_tab_asks_the_mailbox_once_and_says_how_old_its_list_is() {
    let (_home, mut model) = phone();
    heard(&mut model, 0x66, Some("Funkstille"), 7 * 86_400);
    heard(&mut model, 0x77, Some("Knapp"), 7 * 86_400 - 3_600);
    let asks = |effects: &[UiEffect]| {
        effects
            .iter()
            .filter(|e| matches!(e, UiEffect::AskPages))
            .count()
    };
    // The session says the link before the tab opens.
    mailbox(&mut model, true);
    assert_eq!(asks(&tap(&mut model, "tab", "", "", 0)), 1, "opened");
    tap(&mut model, "chats", "", "", 0);
    update(&mut model, UiEvent::Elapsed(40_000));
    assert_eq!(asks(&tap(&mut model, "tab", "", "", 0)), 0, "30 s later");
    let shown = screen(&model);
    assert!(!labels(&shown).contains(&"Funkstille".to_string()));
    assert!(labels(&shown).contains(&"Knapp".to_string()), "control");
    // Fresh: no line. The answer comes at 14:20.
    let answered = NOW - NOW.rem_euclid(86_400) + 14 * 3_600 + 20 * 60;
    clock(&mut model, answered);
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Home(loquax::home::pages::Delta {
            stamp: 1,
            more: false,
            rows: Vec::new(),
        })),
    );
    assert_eq!(screen(&model).as_of, "", "fresh");
    // Three hours on, the link up.
    clock(&mut model, answered + 3 * 3_600);
    let shown = screen(&model);
    assert_eq!(shown.as_of, "Stand: vor 3 Std.");
    check("explore-as-of-old.json", &shown);
    // The next day, the link down.
    mailbox(&mut model, false);
    clock(&mut model, answered + 86_400);
    let shown = screen(&model);
    assert_eq!(shown.as_of, "Nicht verbunden · Stand von gestern 14:20");
    check("explore-as-of-down.json", &shown);
}
