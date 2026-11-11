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
    assert_eq!(
        page.lines.len(),
        page.total as usize,
        "the whole page, which the phone scrolls (1650)"
    );

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

/// A phone with the tab in front, a node it heard and the view's cells.
fn opened() -> (tempfile::TempDir, UiModel<session::Line>) {
    let (home, mut model) = phone();
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
    tap(&mut model, &[("verb", Java::Str("tab"))]);
    tap(
        &mut model,
        &[
            ("verb", Java::Str("size")),
            ("cols", Java::Long(40)),
            ("rows", Java::Long(30)),
        ],
    );
    (home, model)
}

/// Explore's tap on the node, as the phone's Explore sends it.
fn open_node(model: &mut UiModel<session::Line>) -> Vec<UiEffect> {
    update(
        model,
        UiEvent::Explore(loquax::ui::explore::ExploreEvent::OpenPage(NODE)),
    )
}

fn arrive(model: &mut UiModel<session::Line>, micron: &str) {
    update(
        model,
        UiEvent::Browse(BrowseEvent::Loaded {
            micron: micron.to_string(),
            title: "Marcels Knoten".to_string(),
        }),
    );
}

fn numbered(lines: usize, word: &str) -> String {
    (0..lines)
        .map(|i| format!("{word} {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_text(line: &schema::BrowseLine) -> String {
    line.runs.iter().map(|r| r.text.as_str()).collect()
}

/// 1650: the phone takes a page whole, so its own view scrolls it under
/// the finger; the arrows looked dead because the view only ever held a
/// window as tall as itself. A page past the cap comes as a window, and
/// the reader's line, which Java reports with `at`, moves it.
#[test]
fn the_phone_takes_a_page_whole_and_a_very_long_one_as_a_window() {
    let (_home, mut model) = opened();
    open_node(&mut model);
    arrive(&mut model, &numbered(200, "Zeile"));
    let page = screen(&model);
    eprintln!(
        "FACADE whole: lines={} first={} total={} at={} last={:?}",
        page.lines.len(),
        page.first,
        page.total,
        page.at,
        page.lines.last().map(line_text)
    );
    assert_eq!(page.first, 0);
    assert!(page.total >= 200, "{}", page.total);
    assert_eq!(page.lines.len(), page.total as usize, "the whole page");
    assert!(page
        .lines
        .iter()
        .any(|l| line_text(l).contains("Zeile 199")));

    // The finger scrolls; Java says the line; the arrows still work from it.
    tap(
        &mut model,
        &[("verb", Java::Str("at")), ("text", Java::Str("120"))],
    );
    assert_eq!(screen(&model).at, 120);
    assert_eq!(
        screen(&model).lines.len(),
        page.lines.len(),
        "a whole page stays whole"
    );

    // A page past the cap.
    let long = browser::WHOLE_PAGE + 1000;
    open_node(&mut model);
    arrive(&mut model, &numbered(long, "Reihe"));
    let window = screen(&model);
    eprintln!(
        "FACADE windowed: lines={} first={} total={} at={}",
        window.lines.len(),
        window.first,
        window.total,
        window.at
    );
    assert_eq!(window.lines.len(), browser::WHOLE_PAGE);
    assert_eq!(window.first, 0);
    assert!(window.total as usize >= long);
    tap(
        &mut model,
        &[("verb", Java::Str("at")), ("text", Java::Str("3000"))],
    );
    let moved = screen(&model);
    eprintln!(
        "FACADE windowed at 3000: lines={} first={} total={} at={}",
        moved.lines.len(),
        moved.first,
        moved.total,
        moved.at
    );
    assert_eq!(moved.at, 3000);
    assert_eq!(moved.lines.len(), browser::WHOLE_PAGE);
    let first = moved.first as usize;
    assert!(
        first < 3000 && 3000 < first + browser::WHOLE_PAGE - 30,
        "the window holds the reader's line with a screen below it: {first}"
    );
    assert!(line_text(&moved.lines[3000 - first]).contains("Reihe 3000"));

    // The arrows page from the reader's line.
    tap(
        &mut model,
        &[
            ("verb", Java::Str("scroll")),
            ("text", Java::Str("pagedown")),
        ],
    );
    assert!(screen(&model).at > 3000);
}

/// 1650: Explore shows a page from the cache and asks for it again; the
/// reader scrolls the copy, and the fresh one, the same page, keeps the
/// line Java reported. A fresh copy that changed starts at its top.
#[test]
fn a_fresh_copy_keeps_the_readers_line_unless_the_page_changed() {
    let (_home, mut model) = opened();
    let body = numbered(200, "Zeile");
    open_node(&mut model);
    arrive(&mut model, &body);
    // Away to another page, then Explore opens the node again: the cache.
    let effects = open_node(&mut model);
    let refetch = effects
        .iter()
        .any(|e| matches!(e, UiEffect::Browse(lnomad::Effect::Navigate(t)) if t.dest_hash == NODE));
    assert!(refetch, "the cached page is asked for again: {effects:?}");
    assert!(model.browse.model.cached_view, "the copy stands");

    tap(
        &mut model,
        &[("verb", Java::Str("at")), ("text", Java::Str("120"))],
    );
    arrive(&mut model, &body);
    let kept = screen(&model);
    eprintln!(
        "FACADE fresh copy, same page: at={} lines={} cached_view={}",
        kept.at,
        kept.lines.len(),
        model.browse.model.cached_view
    );
    assert!(!model.browse.model.cached_view, "the fresh copy is in");
    assert_eq!(kept.at, 120, "the reader's line holds");

    // The control: the page changed while the copy stood.
    open_node(&mut model);
    tap(
        &mut model,
        &[("verb", Java::Str("at")), ("text", Java::Str("120"))],
    );
    arrive(&mut model, &numbered(200, "Neu"));
    let changed = screen(&model);
    eprintln!("FACADE fresh copy, changed page: at={}", changed.at);
    assert_eq!(changed.at, 0, "a changed page starts at its top");
}

/// 1650: the fields reach Java with their kind, their value and whether
/// they hide what is typed; two text fields filled, a checkbox ticked and
/// a radio picked by `activate` alone, and the submit carries all four.
#[test]
fn a_form_filled_on_the_phone_submits_every_field() {
    let (_home, mut model) = opened();
    open_node(&mut model);
    arrive(
        &mut model,
        "Name: `<12|name`>\nPIN: `<!8|pin`>\n`<?|agree`Einverstanden>\n`<^|farbe|rot`Rot>\n`<^|farbe|blau`Blau>\n`[Senden`:/page/post.mu`*]",
    );
    let page = screen(&model);
    let kinds: Vec<(String, String, bool)> = page
        .fields
        .iter()
        .map(|f| (f.node.clone(), f.kind.clone(), f.masked))
        .collect();
    eprintln!("FACADE fields: {:?}", page.fields);
    assert_eq!(
        kinds,
        [
            ("field_1".to_string(), "text".to_string(), false),
            ("field_2".to_string(), "text".to_string(), true),
            ("field_3".to_string(), "checkbox".to_string(), false),
            ("field_4".to_string(), "radio".to_string(), false),
            ("field_5".to_string(), "radio".to_string(), false),
        ]
    );
    assert_eq!(page.fields[2].label, "Einverstanden");
    assert_eq!(
        page.fields[0].label, "name",
        "a text field goes by its name"
    );

    for (node, text) in [("field_1", "Lew"), ("field_2", "4711")] {
        tap(
            &mut model,
            &[("verb", Java::Str("activate")), ("node", Java::Str(node))],
        );
        tap(
            &mut model,
            &[
                ("verb", Java::Str("fill")),
                ("node", Java::Str(node)),
                ("text", Java::Str(text)),
            ],
        );
    }
    for node in ["field_3", "field_5"] {
        tap(
            &mut model,
            &[("verb", Java::Str("activate")), ("node", Java::Str(node))],
        );
    }
    let filled = screen(&model);
    eprintln!("FACADE filled: {:?} focus={}", filled.fields, filled.focus);
    assert_eq!(filled.fields[0].value, "Lew", "the dialog opens on it");
    assert_eq!(filled.fields[1].value, "4711");
    assert!(filled.fields[2].checked);
    assert!(filled.fields[4].checked && !filled.fields[3].checked);
    assert_eq!(
        filled.focus, "field_5",
        "the focus is drawn where the tap was"
    );

    let send = filled
        .lines
        .iter()
        .flat_map(|l| l.runs.iter())
        .find(|r| r.text.contains("Senden"))
        .expect("the submit link")
        .node
        .clone();
    let effects = tap(
        &mut model,
        &[("verb", Java::Str("activate")), ("node", Java::Str(&send))],
    );
    let fields = effects
        .iter()
        .find_map(|e| match e {
            UiEffect::Browse(lnomad::Effect::Navigate(t)) => Some(t.fields.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a submit: {effects:?}"));
    eprintln!("FACADE submit: {fields:?}");
    for (key, value) in [
        ("field_name", "Lew"),
        ("field_pin", "4711"),
        ("field_agree", "Einverstanden"),
        ("field_farbe", "blau"),
    ] {
        assert!(
            fields.iter().any(|(k, v)| k == key && v == value),
            "{key}={value} in {fields:?}"
        );
    }
}

/// 1650: the bar's actions and chips reach Java as the library lists
/// them; identify switched on the first time says what it means, and the
/// chip carries the key the terminal shows.
#[test]
fn identify_is_an_action_and_its_state_a_chip() {
    let (_home, mut model) = opened();
    model.browse.model.fingerprint = "abcdef0123456789".to_string();
    open_node(&mut model);
    arrive(&mut model, "Hallo");
    let page = screen(&model);
    let verbs: Vec<&str> = page.actions.iter().map(|a| a.verb.as_str()).collect();
    eprintln!("FACADE actions: {:?} chips: {:?}", page.actions, page.chips);
    for verb in ["forward", "reload", "bookmark_on", "identify_on"] {
        assert!(verbs.contains(&verb), "{verb} in {verbs:?}");
    }
    tap(&mut model, &[("verb", Java::Str("identify_on"))]);
    let on = screen(&model);
    eprintln!(
        "FACADE identify on: toast={:?} chips={:?}",
        on.toast, on.chips
    );
    assert_eq!(
        on.toast,
        "Die Seite erfährt deine Adresse, solange das an ist"
    );
    assert!(
        on.chips
            .iter()
            .any(|c| c.starts_with('\u{1F511}') && c.contains("abcdef01")),
        "{:?}",
        on.chips
    );
    assert!(on.actions.iter().any(|a| a.verb == "identify_off"));
    tap(&mut model, &[("verb", Java::Str("identify_off"))]);
    tap(&mut model, &[("verb", Java::Str("identify_on"))]);
    let again = screen(&model);
    assert_ne!(again.toast, on.toast, "the sentence comes once");
}

/// 1650: what a whole 2000-line page costs to cross: the facade's scene
/// and encoding on the Rust side, the bytes JNI copies, and the app's own
/// `MsgPack.java` reading them on the host JVM. Prints the numbers the
/// cap was chosen by; the bound checked is the bytes, which do not depend
/// on the machine.
#[test]
fn a_long_page_crosses_in_time() {
    let (_home, mut model) = opened();
    tap(
        &mut model,
        &[
            ("verb", Java::Str("size")),
            ("cols", Java::Long(48)),
            ("rows", Java::Long(30)),
        ],
    );
    open_node(&mut model);
    let micron = (0..2000)
        .map(|i| format!("`F0af`[Eintrag {i:04}`:/page/p{i}.mu]`f `!fett`! und Text"))
        .collect::<Vec<_>>()
        .join("\n");
    arrive(&mut model, &micron);
    let mut times = Vec::new();
    let mut bytes = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        bytes = browser::encode(&model, Lang::De, 1);
        times.push(start.elapsed());
    }
    times.sort();
    let shown: BrowseScreen = schema::decode(&bytes).expect("the answer");
    eprintln!(
        "RUST_ENCODE bytes={} lines={} median_ms={:.2} (debug build)",
        bytes.len(),
        shown.lines.len(),
        times[2].as_secs_f64() * 1000.0
    );
    assert_eq!(shown.lines.len(), shown.total as usize);
    assert!(shown.lines.len() >= 2000);
    assert!(
        bytes.len() < 4 << 20,
        "a 2000-line page under 4 MB: {}",
        bytes.len()
    );

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("browse-cross");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a class directory");
    let page = dir.join("page.bin");
    std::fs::write(&page, &bytes).expect("the page's bytes");
    let compiled = std::process::Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&dir)
        .arg(manifest.join("android/src/network/leviculum/loquax/MsgPack.java"))
        .arg(manifest.join("tests/java/BrowseCrossTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = std::process::Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&dir)
        .arg("network.leviculum.loquax.BrowseCrossTest")
        .arg(&page)
        .arg(shown.lines.len().to_string())
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    eprintln!("{out}");
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// Walk 11, finding 60 (1667): text graphics on rns.moscow were cut on
/// the right and broke into short and long lines. The phone says its
/// real cells (`size`, from the view's measured width and the font's
/// advance), and the library lays prose out on them; a line the page
/// draws wider than that (a literal block, a run of art without a space)
/// comes whole, as wide as the page drew it, and the phone's view scrolls
/// sideways rather than breaking it (`BrowsePane`'s HorizontalScrollView).
#[test]
fn a_page_wider_than_the_phone_comes_whole_for_the_sideways_scroll() {
    let (_home, mut model) = opened();
    tap(
        &mut model,
        &[
            ("verb", Java::Str("size")),
            ("cols", Java::Long(48)),
            ("rows", Java::Long(30)),
        ],
    );
    open_node(&mut model);
    let art: String = "\u{28ff}\u{2800}".repeat(60);
    let boxed = format!("+{}+", "-".repeat(118));
    let prose = "word ".repeat(40);
    let micron = format!("`=\n{boxed}\n`=\n{art}\n{}\n", prose.trim_end());
    arrive(&mut model, &micron);
    let page = screen(&model);
    let widths: Vec<usize> = page
        .lines
        .iter()
        .map(|l| line_text(l).chars().count())
        .collect();
    eprintln!("FACADE wide: widths={widths:?}");
    let texts: Vec<String> = page.lines.iter().map(line_text).collect();
    assert!(
        texts.iter().any(|t| t.trim_end() == boxed),
        "the literal box line whole: {texts:#?}"
    );
    assert!(
        texts.iter().any(|t| t.trim() == art),
        "the art run whole: {texts:#?}"
    );
    // Prose still wraps on the phone's cells: no prose line past 48.
    for t in texts.iter().filter(|t| t.contains("word")) {
        assert!(t.chars().count() <= 48, "prose wraps at 48: {t:?}");
    }
}

/// Walk 11, finding 65 (1667): a page opened from Explore a second time
/// looked refetched. Explore's tap goes the cache's way as the browser's
/// Back does: the cached copy at once, its lines on screen and the chip
/// saying so, and the fresh copy asked behind it.
#[test]
fn a_page_opened_again_from_explore_shows_the_cached_copy_first() {
    let (_home, mut model) = opened();
    open_node(&mut model);
    arrive(&mut model, &numbered(5, "Zeile"));
    let first = screen(&model);
    assert!(first.lines.iter().any(|l| line_text(l).contains("Zeile 4")));
    // Back to Explore and the same page again.
    update(&mut model, UiEvent::Tab(loquax::ui::scene::TabId::Explore));
    let effects = open_node(&mut model);
    let again = screen(&model);
    eprintln!(
        "FACADE again: loading={} chips={:?} lines={} effects={effects:?}",
        again.loading,
        again.chips,
        again.lines.len()
    );
    assert!(
        again.lines.iter().any(|l| line_text(l).contains("Zeile 4")),
        "the cached lines at once: {:?}",
        again.lines.iter().map(line_text).collect::<Vec<_>>()
    );
    assert!(
        again
            .chips
            .iter()
            .any(|c| c.contains("aus dem Seitenspeicher")),
        "the chip says the cache: {:?}",
        again.chips
    );
    assert!(
        again.status.contains("wird aktualisiert"),
        "the fresh copy comes behind it: {:?}",
        again.status
    );
}
