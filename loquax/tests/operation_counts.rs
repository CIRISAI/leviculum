//! The bars of "only what is visible" and "O(1) per event" (1489, structure
//! review 2026-11-07 finding 14), on the core and on the terminal's pass,
//! over the real fixture (`support/real_store.rs`): counted with the core's
//! operation counters ([`loquax::ui::counters`]), never timed, so they hold
//! on a loaded host as on a quiet one.
//!
//! Each test prints its counts (`COUNTS <bar> <counters>`, the report's
//! table) and judges its bar through the register in the fixture: red at
//! the commit that wrote them, each one green with the plan step that
//! fixes it (B: the scene and the layout built for what is on screen; C:
//! nothing built without a change).

#[path = "support/real_store.rs"]
mod real_store;

use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers, Line};
use loquax::outbox::OutboxEvent;
use loquax::store::Store;
use loquax::ui::counters::Counters;
use loquax::ui::history::{Restored, LOAD_WINDOW};
use loquax::ui::scene::LayoutClass;
use loquax::ui::{update, Pane, UiEvent, UiModel};

use real_store::{bar, ANNA, CONVERSATIONS};

/// The list rows a phone's screen shows, and as many again as margin.
const LIST_ON_SCREEN: u32 = 14;
/// The bubbles a tall window shows, and as many again as margin.
const BUBBLES_ON_SCREEN: u32 = 24;
/// The terminal's screen in this test.
const SCREEN: (u16, u16) = (120, 40);

fn counted<B>(model: &UiModel<B>) -> Counters {
    model.counters().expect("counting() said this build counts")
}

fn key(code: KeyCode) -> UiEvent {
    UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn received(peer: [u8; 16], n: u32, body: &str) -> UiEvent {
    let mut id = [0xc4; 32];
    id[..4].copy_from_slice(&n.to_be_bytes());
    UiEvent::Engine(OutboxEvent::Received {
        sent_at: real_store::BASE + 600 + i64::from(n),
        message_id: id,
        source: peer,
        title: Vec::new(),
        body: body.as_bytes().to_vec(),
        fields: Vec::new(),
        verified: true,
    })
}

/// The fixture is shaped as its table says, and the model holds it whole.
#[test]
fn the_real_store_is_shaped_like_lews() {
    real_store::check_shape();
    let model: UiModel<Line> = real_store::model();
    assert_eq!(model.conversations.len(), CONVERSATIONS);
    let held: usize = model.conversations.iter().map(|c| c.messages.len()).sum();
    assert_eq!(held, real_store::MESSAGES);
}

/// A key in the list (the desktop's, list beside the open chat) builds
/// the list rows on screen and the next chat's bubbles on screen, each
/// with a margin; today it builds all 300 rows and the whole next chat.
#[test]
fn a_key_in_the_list_builds_the_rows_on_screen() {
    if !real_store::counting() {
        return;
    }
    let mut model: UiModel<Line> = real_store::model();
    model.focus = Pane::Conversations;
    update(&mut model, key(KeyCode::Down));
    let _ = model.scene(LayoutClass::Wide);
    let counts = counted(&model);
    println!("COUNTS core.list_key {counts}");
    bar(
        "core.list_key_builds_the_rows_on_screen",
        counts.rows_built <= 2 * LIST_ON_SCREEN && counts.bubbles_built <= 2 * BUBBLES_ON_SCREEN,
        &format!(
            "rows_built {} (bar {}), bubbles_built {} (bar {})",
            counts.rows_built,
            2 * LIST_ON_SCREEN,
            counts.bubbles_built,
            2 * BUBBLES_ON_SCREEN
        ),
    );
}

/// A message arriving in a closed chat builds nothing of the open one:
/// the long chat open, a friend writes.
#[test]
fn a_message_in_a_closed_chat_builds_nothing_of_the_open_one() {
    if !real_store::counting() {
        return;
    }
    let mut model: UiModel<Line> = real_store::model();
    update(
        &mut model,
        UiEvent::Open {
            peer: real_store::peer(ANNA),
        },
    );
    update(
        &mut model,
        received(real_store::peer(12), 1, "Bist du heute Abend da?"),
    );
    let _ = model.scene(LayoutClass::Wide);
    let counts = counted(&model);
    println!("COUNTS core.closed_chat_message {counts}");
    bar(
        "core.a_message_in_a_closed_chat_builds_nothing_of_the_open_one",
        counts.bubbles_built == 0,
        &format!("bubbles_built {} (bar 0)", counts.bubbles_built),
    );
}

/// A frame no event asked for builds nothing: the scene asked twice.
#[test]
fn a_frame_without_a_change_builds_nothing() {
    if !real_store::counting() {
        return;
    }
    let mut model: UiModel<Line> = real_store::model();
    update(
        &mut model,
        UiEvent::Open {
            peer: real_store::peer(ANNA),
        },
    );
    let _ = model.scene(LayoutClass::Wide);
    model.reset_counters();
    let _ = model.scene(LayoutClass::Wide);
    let counts = counted(&model);
    println!("COUNTS core.idle_frame {counts}");
    bar(
        "core.a_frame_without_a_change_builds_nothing",
        counts.scene_nodes == 0 && counts.rows_built == 0 && counts.bubbles_built == 0,
        &format!(
            "scene_nodes {} rows_built {} bubbles_built {} (bar 0)",
            counts.scene_nodes, counts.rows_built, counts.bubbles_built
        ),
    );
}

/// The work units of a count, for a comparison of two sizes.
fn work(c: &Counters) -> u32 {
    c.scene_nodes + c.rows_built + c.bubbles_built + c.rows_laid + c.sorted
}

/// A smaller history of the same shape: the first 40 conversations, each
/// with its newest seventh (at least one), 415 messages.
fn small() -> Vec<Restored> {
    real_store::restored()
        .into_iter()
        .take(40)
        .map(|mut r| {
            let keep = r.messages.len().div_ceil(7);
            let cut = r.messages.len() - keep;
            r.messages.drain(..cut);
            r.unread = r.unread.min(keep);
            r
        })
        .collect()
}

/// A model over `restored`, the long chat open.
fn model_of(restored: Vec<Restored>) -> UiModel<Line> {
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some(real_store::own());
    for (peer, name) in real_store::names() {
        model.names.remember(peer, &name);
    }
    update(&mut model, UiEvent::Clock(real_store::now()));
    model.restore(restored);
    update(
        &mut model,
        UiEvent::Open {
            peer: real_store::peer(ANNA),
        },
    );
    model
}

/// A pull's page, forty messages to ten friends, each folded and drawn as
/// the window's pump does: what it costs over the whole page.
fn page(model: &mut UiModel<Line>) -> Counters {
    let mut total = Counters::default();
    for n in 0..40u32 {
        let friend = real_store::FRIENDS.start + (n as usize % 10);
        update(
            model,
            received(real_store::peer(friend), 1_000 + n, "Neue Nachricht"),
        );
        let _ = model.scene(LayoutClass::Wide);
        total = total + counted(model);
    }
    total
}

/// A pull page costs the same over 415 entries as over 4000, within 2x:
/// the per-event work does not grow with the store.
#[test]
fn a_pull_page_costs_the_same_at_400_and_4000() {
    if !real_store::counting() {
        return;
    }
    let small = small();
    let entries: usize = small.iter().map(|r| r.messages.len()).sum();
    let mut few = model_of(small);
    let mut many = model_of(real_store::restored());
    let (a, b) = (page(&mut few), page(&mut many));
    println!("COUNTS core.pull_page_{entries} {a}");
    println!("COUNTS core.pull_page_{} {b}", real_store::MESSAGES);
    bar(
        "core.a_pull_page_costs_the_same_at_400_and_4000",
        work(&b) <= 2 * work(&a),
        &format!(
            "work {} at {entries} entries, {} at {} (bar 2x)",
            work(&a),
            work(&b),
            real_store::MESSAGES
        ),
    );
}

/// A restart reads the list's summaries and the open chat's page, no
/// more: the store's reads at start, as the shell makes them
/// (`loquax/src/shell.rs`, `start`: restore, names, last key, positions,
/// held signatures, the tracks, the propagation node's asks).
#[test]
fn a_restart_reads_the_summaries() {
    if !real_store::counting() {
        return;
    }
    let home = tempfile::tempdir().expect("home");
    let filled = real_store::fill_store(home.path());
    let commits = filled.counters().expect("counted").commits;
    drop(filled);
    let store = Store::open_in(home.path(), real_store::own()).expect("the store");
    store.reset_counters();
    let restored = store.restore().expect("restore");
    let names = store.names().expect("names");
    store.last_key().expect("last key");
    store.positions().expect("positions");
    store.unverified().expect("held");
    store
        .fixes_between(None, i64::MIN, i64::MAX)
        .expect("tracks");
    store.pn_asks().expect("asks");
    let counts = store.counters().expect("counted");
    println!("COUNTS core.restart {counts} (fill: {commits} commits)");
    assert_eq!(restored.len(), CONVERSATIONS);
    let most = (CONVERSATIONS + names.len() + LOAD_WINDOW) as u32;
    bar(
        "core.a_restart_reads_the_summaries",
        counts.store_rows_read <= most,
        &format!(
            "store_rows_read {} (bar {most}: the summaries, the names, one page)",
            counts.store_rows_read
        ),
    );
}

/// The terminal's pass over `model`: what `loquax/src/tui.rs` asks of the
/// core on every turn of its loop (`say_viewport`'s viewport, then the
/// view it draws).
fn terminal_pass(model: &UiModel<Line>) {
    let _ = loquax::ui::hit::viewport(model);
    let _ = loquax::ui::view(model);
}

/// The long chat open in the terminal, the keys in its messages.
fn terminal_model() -> UiModel<Line> {
    let mut model: UiModel<Line> = real_store::model();
    update(
        &mut model,
        UiEvent::Resize {
            width: SCREEN.0,
            height: SCREEN.1,
        },
    );
    update(
        &mut model,
        UiEvent::Open {
            peer: real_store::peer(ANNA),
        },
    );
    model.focus = Pane::Messages;
    terminal_pass(&model);
    model
}

/// An arrow key in the terminal's messages lays the rows on screen, and
/// as many again; today it lays the whole chat three times.
#[test]
fn a_key_in_the_terminals_messages_lays_the_rows_on_screen() {
    if !real_store::counting() {
        return;
    }
    let mut model = terminal_model();
    update(&mut model, key(KeyCode::Up));
    terminal_pass(&model);
    let counts = counted(&model);
    println!("COUNTS terminal.messages_key {counts}");
    let most = 2 * u32::from(SCREEN.1);
    bar(
        "terminal.a_key_in_the_messages_lays_the_rows_on_screen",
        counts.rows_laid <= most,
        &format!("rows_laid {} (bar {most})", counts.rows_laid),
    );
}

/// A turn of the terminal's loop with nothing changed lays nothing.
#[test]
fn a_terminal_pass_without_a_change_lays_nothing() {
    if !real_store::counting() {
        return;
    }
    let model = terminal_model();
    model.reset_counters();
    terminal_pass(&model);
    let counts = counted(&model);
    println!("COUNTS terminal.idle_pass {counts}");
    bar(
        "terminal.a_pass_without_a_change_lays_nothing",
        counts.rows_laid == 0 && counts.rows_built == 0,
        &format!(
            "rows_laid {} rows_built {} (bar 0)",
            counts.rows_laid, counts.rows_built
        ),
    );
}

/// The register's own arms, as the guardrail rule asks: a green bar
/// passes, a known red passes and a red the register does not name fails,
/// as a green one it names does.
#[test]
fn the_register_judges_both_ways() {
    bar("a bar nobody registered", true, "green");
    bar(real_store::KNOWN_REDS[0], false, "still red");
    let unknown_red = std::panic::catch_unwind(|| bar("a bar nobody registered", false, "red"));
    assert!(unknown_red.is_err(), "an unregistered red passed");
    let known_green = std::panic::catch_unwind(|| bar(real_store::KNOWN_REDS[0], true, "green"));
    assert!(
        known_green.is_err(),
        "a registered bar went green unnoticed"
    );
}
