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
use loquax::ui::{update, Pane, Presented, UiEvent, UiModel};

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
    // The frame the open drew, as every shell draws one: since 1591 the
    // chat's window is built by the first scene read after a note, and
    // without this frame the message's count paid for the open's.
    let _ = model.scene(LayoutClass::Wide);
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

/// A late message in the open long chat (1505): written half a year
/// back, handed on by the mailbox now, it is spliced in at its writer's
/// time, the conversation's revision moves once, and the one scene built
/// for it builds the list's window and the chat's, not the 1200 bubbles.
/// The summary's ids are read again without a walk counted.
#[test]
fn a_late_message_in_the_open_chat_is_one_splice_and_one_build() {
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
    let c = model.selected;
    let len = model.conversations[c].messages.len();
    let revision = model.conversations[c].revision();
    let half = &model.conversations[c].messages[len / 2];
    let sent_at = half.at.expect("a time").unix_secs;
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at,
            message_id: [0xd5; 32],
            source: real_store::peer(ANNA),
            title: Vec::new(),
            body: b"late from the mailbox".to_vec(),
            fields: Vec::new(),
            verified: true,
        }),
    );
    let _ = model.scene(LayoutClass::Wide);
    let counts = counted(&model);
    println!("COUNTS core.late_message {counts}");
    let conversation = &model.conversations[c];
    let at = conversation
        .messages
        .iter()
        .position(|m| m.body == "late from the mailbox")
        .expect("placed");
    assert!(
        at <= len / 2 + 1 && at + 1 < conversation.messages.len(),
        "at its writer's time, not at the end: {at} of {}",
        conversation.messages.len()
    );
    assert_eq!(conversation.revision(), revision + 1, "one revision");
    bar(
        "core.a_late_message_is_one_splice_and_one_build",
        counts.scenes == 1
            && counts.bubbles_built <= 2 * BUBBLES_ON_SCREEN
            && counts.rows_built <= 2 * LIST_ON_SCREEN
            && counts.walked == 0,
        &format!(
            "scenes {} (bar 1), bubbles_built {} (bar {}), rows_built {} (bar {}), walked {} (bar 0)",
            counts.scenes,
            counts.bubbles_built,
            2 * BUBBLES_ON_SCREEN,
            counts.rows_built,
            2 * LIST_ON_SCREEN,
            counts.walked
        ),
    );
}

/// Five seconds of the window's loop left alone build nothing (1491):
/// the clock's events fold, the gate asks the model's revision, and the
/// scene is built only when it moved. The control is the rule before
/// 1491, a scene built on every pass: 250 of them in the five seconds,
/// each with its list and its chat.
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
    let (gated, counts) = idle_builds(&mut model, true);
    println!("COUNTS core.idle_frame builds={gated} {counts}");
    let (every, control) = idle_builds(&mut model, false);
    println!("COUNTS core.idle_frame_control builds={every} {control}");
    assert!(
        every >= 200 && control.scene_nodes > 0,
        "the control built nothing: {every} builds, {control}"
    );
    bar(
        "core.a_frame_without_a_change_builds_nothing",
        gated == 0 && counts.scene_nodes == 0 && counts.rows_built == 0,
        &format!(
            "builds {gated} scene_nodes {} rows_built {} bubbles_built {} over {} ms (bar 0)",
            counts.scene_nodes,
            counts.rows_built,
            counts.bubbles_built,
            real_store::IDLE_MS
        ),
    );
}

/// The scenes the window's loop builds over [`real_store::idle`], gated
/// on the revision (`gated`) or on every pass, and what they built: the
/// gate's first build (the frame the loop drew before it was left alone)
/// is made before the five seconds.
fn idle_builds(model: &mut UiModel<Line>, gated: bool) -> (u64, Counters) {
    let mut presented = Presented::default();
    let _ = presented.due(Presented::stamp(model.revision, &[]));
    let mut builds = 0;
    let mut total = Counters::default();
    real_store::idle(model, 0, |model| {
        if !gated || presented.due(Presented::stamp(model.revision, &[])) {
            builds += 1;
            let _ = model.scene(LayoutClass::Wide);
        }
        total = total + counted(model);
    });
    (builds, total)
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
    let restored = store.start().expect("the start read");
    let names = store.names().expect("names");
    store.last_key().expect("last key");
    let positions = store.positions().expect("positions");
    store.unverified().expect("held");
    let tracks = store
        .fixes_between(None, i64::MIN, i64::MAX)
        .expect("tracks");
    store.pn_asks().expect("asks");
    let counts = store.counters().expect("counted");
    println!("COUNTS core.restart {counts} (fill: {commits} commits)");
    assert_eq!(restored.len(), CONVERSATIONS);
    let bodies: usize = restored.iter().map(|r| r.messages.len()).sum();
    assert_eq!(bodies, 0, "the start read hands summaries, no body");
    // Every row read is one the model keeps (the summaries, the names,
    // the positions, the tracks), plus at most one page of slack for a
    // conversation whose listing a foreign write nulled: no read
    // amplification. Before schema 17 the start read every
    // conversation's newest page, 4471 rows against this fixture.
    let most = (CONVERSATIONS + names.len() + positions.len() + tracks.len() + LOAD_WINDOW) as u32;
    bar(
        "core.a_restart_reads_the_summaries",
        counts.store_rows_read <= most,
        &format!(
            "store_rows_read {} (bar {most}: the summaries, the names, the state, one page)",
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

/// Five seconds of the terminal's loop left alone lay and build nothing
/// (1491): the loop (`loquax/src/tui.rs`) asks its gate with the model's
/// revision and the screen's size before `say_viewport`, the view and
/// the draw. The control asks no gate: the view's own cache (1490) is
/// keyed on the revision, which before 1491 every pump moved.
#[test]
fn a_terminal_pass_without_a_change_lays_nothing() {
    if !real_store::counting() {
        return;
    }
    let mut model = terminal_model();
    let (gated, counts) = terminal_idle(&mut model, true);
    println!("COUNTS terminal.idle_pass builds={gated} {counts}");
    let (every, _) = terminal_idle(&mut model, false);
    println!("COUNTS terminal.idle_pass_control passes={every}");
    assert!(every >= 200, "the control passed {every} times");
    bar(
        "terminal.a_pass_without_a_change_lays_nothing",
        gated == 0 && counts.rows_laid == 0 && counts.rows_built == 0,
        &format!(
            "passes {gated} rows_laid {} rows_built {} over {} ms (bar 0)",
            counts.rows_laid,
            counts.rows_built,
            real_store::IDLE_MS
        ),
    );
}

/// The terminal's passes over [`real_store::idle`], gated as the loop
/// gates them or on every turn, and what they built.
fn terminal_idle(model: &mut UiModel<Line>, gated: bool) -> (u64, Counters) {
    let stamp = |model: &UiModel<Line>| {
        Presented::stamp(
            model.revision,
            &[u64::from(model.screen.0), u64::from(model.screen.1)],
        )
    };
    let mut presented = Presented::default();
    let _ = presented.due(stamp(model));
    let mut passes = 0;
    let mut total = Counters::default();
    real_store::idle(model, 0, |model| {
        if !gated || presented.due(stamp(model)) {
            passes += 1;
            terminal_pass(model);
        }
        total = total + counted(model);
    });
    (passes, total)
}

/// The register's own arms, as the guardrail rule asks: a green bar
/// passes, a known red passes and a red the register does not name fails,
/// as a green one it names does.
#[test]
fn the_register_judges_both_ways() {
    // A register of its own (1600): the real one may be empty.
    const REGISTER: &[&str] = &["a registered bar"];
    let judged = |name, ok, what| real_store::judged(REGISTER, name, ok, what);
    judged("a bar nobody registered", true, "green");
    judged(REGISTER[0], false, "still red");
    let unknown_red = std::panic::catch_unwind(|| judged("a bar nobody registered", false, "red"));
    assert!(unknown_red.is_err(), "an unregistered red passed");
    let known_green = std::panic::catch_unwind(|| judged(REGISTER[0], true, "green"));
    assert!(
        known_green.is_err(),
        "a registered bar went green unnoticed"
    );
}

/// The terminal over a chat of 3000 one-line bubbles (1487, the window
/// bench's long chat): a scroll key, a page and a pass without a change
/// lay the rows on screen and as many again at most, as in the real
/// store's chat ([`a_key_in_the_terminals_messages_lays_the_rows_on_screen`]);
/// a message that arrives in it is printed with what it laid.
#[test]
fn the_terminal_lays_the_rows_on_screen_in_a_chat_of_3000() {
    if !real_store::counting() {
        return;
    }
    let peer = [0x3a; 16];
    let mut model: UiModel<Line> = real_store::model();
    for n in 0..3000u32 {
        update(
            &mut model,
            received(
                peer,
                n,
                &format!("Message {n}, a line about the market and the apples."),
            ),
        );
    }
    update(
        &mut model,
        UiEvent::Resize {
            width: SCREEN.0,
            height: SCREEN.1,
        },
    );
    update(&mut model, UiEvent::Open { peer });
    model.focus = Pane::Messages;
    terminal_pass(&model);
    let most = 2 * u32::from(SCREEN.1);
    for (name, event) in [
        ("up", key(KeyCode::Up)),
        ("page_up", key(KeyCode::PageUp)),
        ("page_up_again", key(KeyCode::PageUp)),
        ("down", key(KeyCode::Down)),
    ] {
        update(&mut model, event);
        terminal_pass(&model);
        let counts = counted(&model);
        println!("COUNTS terminal.long_chat_{name} {counts}");
        bar(
            &format!("terminal.long_chat_{name}_lays_the_rows_on_screen"),
            counts.rows_laid <= most && counts.bubbles_built == 0,
            &format!(
                "rows_laid {} (bar {most}), bubbles_built {} (bar 0)",
                counts.rows_laid, counts.bubbles_built
            ),
        );
    }
    model.reset_counters();
    terminal_pass(&model);
    let counts = counted(&model);
    println!("COUNTS terminal.long_chat_pass {counts}");
    bar(
        "terminal.long_chat_pass_lays_nothing",
        counts.rows_laid == 0,
        &format!("rows_laid {} (bar 0)", counts.rows_laid),
    );
    update(&mut model, received(peer, 5000, "a new one"));
    terminal_pass(&model);
    println!("COUNTS terminal.long_chat_arrival {}", counted(&model));
}
