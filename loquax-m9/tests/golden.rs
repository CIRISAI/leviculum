//! The M9's screens drawn on the host (1351, 1356, `handheld-messenger.md`,
//! "The test plan", item 1): the ugly store's model (1173), its Compact
//! scene, drawn by the M9's renderer through mousefood into the 320 x 240
//! RGB565 frame, and pinned as a PNG under `tests/golden/`, compared in
//! bytes. The chats list (slice one), and the open chat, a group's chat
//! and the compose line typed into with the M9's keys (slice two). A frame that moved fails and leaves the new picture beside the
//! test's target dir as `m9-golden/<name>`; `LOQUAX_WRITE_GOLDEN=1` writes
//! the goldens instead, for a change that means to move them.
//!
//! Every frame is compared after a trip through the board's dump
//! (`loquax_m9::dump`, slice four), so a golden here is also what a dump of
//! that frame reads back as, byte for byte.

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

mod support;

use std::path::Path;

use support::check;

use leviculum_lxmf::lev::{
    DigestLine, GroupCopy, Invite, Lev, Member, Members, CAP_GROUPS, CAP_READ_RECEIPTS,
    MEMBER_ADMIN, MEMBER_JOINED, MEMBER_LEFT,
};
use leviculum_ui::input::InputBuffer;
use leviculum_ui::style::RLine;
use leviculum_ui::text::fit_width;
use loquax::outbox::OutboxEvent;
use loquax::store::Store;
use loquax::ui::scene::{LayoutClass, Scene};
use loquax::ui::strings::Lang;
use loquax::ui::{update, Pane, UiEffect, UiEvent, UiModel, NONE_OPEN};
use loquax::when::WallTime;
use loquax_m9::field::Field;
use loquax_m9::font::CELL_PX;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::kbraw::{self, Read, MEASURED};
use loquax_m9::keyboard::Key;
use loquax_m9::keys::{event, M9Key};
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look, COLS, ENTRY_ROWS, ROWS};

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
fn ugly_model(home: &Path) -> UiModel<Field> {
    let own = ugly_store::identity(OWN);
    ugly_store::fill_store_of(home, &own);
    let me = loquax::engine::delivery_address(&own).expect("address");
    let store = Store::open_in(home, me).expect("the store");
    let contacts = loquax::names::load_contacts(home).expect("contacts");
    let mut model = UiModel::<Field>::new(&[]);
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
    draw_in(scene, &look())
}

/// `scene` drawn in `look` into a fresh frame by a fresh renderer.
fn draw_in(scene: &Scene, look: &Look) -> Frame {
    let mut frame = Frame::new();
    Renderer::new(&mut frame)
        .expect("renderer")
        .draw(scene, look)
        .expect("draw");
    frame
}

/// What `model` shows on the M9, drawn fresh: its scene in its look.
fn shown(model: &UiModel<Field>) -> Frame {
    draw_in(&screen::scene(model), &Look::of(model))
}

/// The screen's characters, a string a row, as the cells hold them.
fn text_of(model: &UiModel<Field>) -> Vec<String> {
    let lines = screen::lines(&screen::scene(model), &Look::of(model), COLS, ROWS);
    lines.iter().map(row_text).collect()
}

fn row_text(line: &RLine) -> String {
    line.cells.iter().map(|c| c.ch).collect()
}

/// Press `key` on the M9.
fn press(model: &mut UiModel<Field>, key: M9Key) -> Vec<UiEffect> {
    update(model, UiEvent::Key(event(key)))
}

/// Press `key` as the board does ([`loquax_m9::group::press`]): the
/// chat's scroll keys go by the M9's own lines (1529).
fn on_the_board(model: &mut UiModel<Field>, key: M9Key) -> Vec<UiEffect> {
    let mut row = 0;
    loquax_m9::group::press(model, &mut row, &event(key))
}

fn type_in(model: &mut UiModel<Field>, text: &str) {
    for c in text.chars() {
        press(model, M9Key::Char(c));
    }
}

/// The ugly store's model with the list's cursor on the pinned
/// conversation and the keys on the list, as the M9 starts.
fn on_the_list(home: &Path) -> UiModel<Field> {
    let mut model = ugly_model(home);
    model.selected = ugly_store::PINNED;
    model.focus = Pane::Conversations;
    model
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
    // The top bar is one cell row, each entry ENTRY_ROWS more.
    let cell = usize::from(CELL_PX.1);
    let band = (cell + row * ENTRY_ROWS * cell)..(cell + (row + 1) * ENTRY_ROWS * cell);
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

/// Slice two: Enter on the list opens the chat under the cursor, the
/// newest at the bottom over the compose line's hint; Up twelve times
/// moves the cursor over the bubbles and, past the first screen, the
/// window with it. Every row is a cell a character: the store's emoji and Japanese are
/// the atlas's stand-in.
#[test]
fn the_open_chat_is_the_golden_frame() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    let list = screen::scene(&model);
    assert!(
        list.chat.is_none() && list.list.is_some(),
        "the keys on the list: the list"
    );
    press(&mut model, M9Key::Enter);
    assert_eq!(model.focus, Pane::Messages);
    let scene = screen::scene(&model);
    assert!(
        scene.chat.is_some() && scene.list.is_none(),
        "the keys in the chat: the chat alone"
    );
    let rows = text_of(&model);
    for row in &rows {
        assert!(
            row.chars()
                .all(|c| fit_width(c.encode_utf8(&mut [0; 4])) == 1),
            "{row:?} has a character the atlas cannot set in one cell"
        );
    }
    assert!(
        rows.iter().any(|r| r.contains('?')),
        "the store's emoji drawn as the stand-in: {rows:#?}"
    );
    check("m9-chat.png", &shown(&model));
    // As the board presses them (1529): twelve lines up, the selection
    // following the lines.
    for _ in 0..12 {
        on_the_board(&mut model, M9Key::Up);
    }
    let moved = text_of(&model);
    assert_ne!(moved, rows, "twelve lines up, the window moved");
    assert!(
        moved.iter().any(|r| r.contains(screen::BAR)),
        "the cursor's bubble behind the bar: {moved:#?}"
    );
    check("m9-chat-cursor.png", &shown(&model));
    // Back from the chat: the list, its cursor on the chat.
    press(&mut model, M9Key::Back);
    assert_eq!(model.focus, Pane::Conversations);
    assert!(screen::scene(&model).list.is_some());
}

/// 1529 on the M9: a short message, one over three screens long, a short
/// one, pressed as the board presses. From the top the ring's down shows
/// one new line a press and keeps the long one selected while its lines
/// come; the frame twenty presses in is its middle (pinned,
/// `m9-chat-long-mid.png`); right pages a screen less a line; back from
/// the newest the ring's up enters it at its end (pinned,
/// `m9-chat-long-back.png`), not at its start.
#[test]
fn a_long_message_is_read_by_the_line_on_the_m9() {
    let peer = [0x4f; 16];
    let mut model = UiModel::<Field>::new(&[peer]);
    update(&mut model, UiEvent::Viewing(screen::viewing()));
    update(&mut model, UiEvent::Clock(now()));
    let long: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    for (n, body) in ["short before", &long.join("\n"), "short after"]
        .into_iter()
        .enumerate()
    {
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [n as u8 + 1; 32],
                source: peer,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: Vec::new(),
                verified: true,
            }),
        );
    }
    model.toasts.clear();
    model.focus = Pane::Conversations;
    on_the_board(&mut model, M9Key::Enter);
    assert_eq!(model.focus, Pane::Messages);
    let at_end = text_of(&model);
    assert!(
        at_end.iter().any(|r| r.contains("short after")),
        "{at_end:#?}"
    );
    // To the top: up until the first message is selected and its line on.
    for _ in 0..80 {
        on_the_board(&mut model, M9Key::Up);
    }
    assert_eq!(model.message_cursor, 0);
    let top = text_of(&model);
    assert!(top.iter().any(|r| r.contains("short before")), "{top:#?}");
    // Down a line a press: every line of the long one shows, none skipped.
    let mut seen = std::collections::BTreeSet::new();
    let mut last_line = |rows: &[String]| {
        for r in rows {
            if let Some(n) = r.trim().strip_prefix("line ").or_else(|| {
                r.trim()
                    .strip_prefix(screen::BAR)
                    .and_then(|t| t.trim().strip_prefix("line "))
            }) {
                let n: usize = n
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                seen.insert(n);
            }
        }
    };
    last_line(&top);
    let mut mid = None;
    for press in 1..=60 {
        on_the_board(&mut model, M9Key::Down);
        let rows = text_of(&model);
        last_line(&rows);
        if press == 20 {
            assert_eq!(model.message_cursor, 1, "the long one, its end not on yet");
            mid = Some(shown(&model));
        }
    }
    assert_eq!(
        seen,
        (1..=40).collect(),
        "every line of the long one showed"
    );
    assert_eq!(model.message_cursor, 2, "the short one under it at the end");
    check("m9-chat-long-mid.png", &mid.expect("twenty presses in"));
    // Back up from the newest: the long one's end, a line a press.
    on_the_board(&mut model, M9Key::Up);
    on_the_board(&mut model, M9Key::Up);
    assert_eq!(model.message_cursor, 1);
    let back = text_of(&model);
    assert!(back.iter().any(|r| r.contains("line 40")), "{back:#?}");
    assert!(!back.iter().any(|r| r.contains("line 1 ")), "{back:#?}");
    check("m9-chat-long-back.png", &shown(&model));
    // Left pages up a screen less a line: the line that was on top is at
    // the bottom of the bubbles now.
    let first = back
        .iter()
        .find(|r| r.contains("line "))
        .cloned()
        .expect("a line on screen");
    on_the_board(&mut model, M9Key::Left);
    let paged = text_of(&model);
    let at = paged
        .iter()
        .position(|r| *r == first)
        .expect("still on screen");
    assert!(
        paged[at + 1..].iter().all(|r| !r.contains("line ")),
        "the overlap line is the last line shown: {paged:#?}"
    );
    // A held ring key: the controller answers its code again on the polls
    // it holds (1463), and each answer is a press, so a hold scrolls a
    // line an answer; the board keeps no timer of its own for it.
    let line = paged
        .iter()
        .position(|r| r.contains("line "))
        .expect("a line");
    let held = paged[line].clone();
    for _ in 0..3 {
        let Read::Code(code) = MEASURED.read(&[0xb5; 8]) else {
            panic!("a ring answer is a code");
        };
        let event = Key::of(code).and_then(Key::event).expect("the ring's up");
        let mut row = 0;
        loquax_m9::group::press(&mut model, &mut row, &event);
    }
    let after = text_of(&model);
    assert_eq!(
        after[line + 3],
        held,
        "three answers, three lines: {after:#?}"
    );
}

/// 1544, the bench walk's black chat (window 11a, dump 03): a chat like
/// "Lew Mobile", a run of position reports folded into one "last position"
/// bubble and nine short texts after it, opened at the newest and the
/// ring's up held three times, ten answers 0xb5 each as the controller
/// gives them. The held
/// up climbs to the folded run, the chat's first bubble; the pane then
/// shows that bubble at its top and the texts under it, never the bar
/// alone at the bottom over a black body. Pinned, `m9-chat-top-held.png`.
#[test]
fn a_held_ring_up_to_the_chats_top_still_shows_the_chat() {
    let peer = [0x4c; 16];
    let mut model = UiModel::<Field>::new(&[peer]);
    update(&mut model, UiEvent::Viewing(screen::viewing()));
    clock(&mut model, 0);
    let receive = |model: &mut UiModel<Field>, id: u8, body: &str, fields| {
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [id; 32],
                source: peer,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields,
                verified: true,
            }),
        );
    };
    // Eleven reports in a day fold into one bubble, the run's newest.
    for n in 0..11u8 {
        clock(&mut model, i64::from(n) * 60);
        let at = T0 + i64::from(n) * 60;
        receive(
            &mut model,
            n + 1,
            "",
            ugly_store::position(at, 48_137_000 + i32::from(n) * 37),
        );
    }
    let texts = [
        "Hier Lew. Die Nachricht habe ich erhalten.",
        "Offline-Test UI-Walk",
        "test",
        "test an coluimba",
        "noch einer",
        "und noch einer, etwas laenger als eine Zeile",
        "kurz",
        "fast unten",
        "der neueste",
    ];
    for (n, body) in texts.into_iter().enumerate() {
        // An hour apart: each its own group, a blank row between.
        clock(&mut model, 3_600 * (n as i64 + 1));
        receive(&mut model, 0x40 + n as u8, body, Vec::new());
    }
    model.toasts.clear();
    // Whatever the route, a pane at the chat's first bubble shows it at
    // its top and the rows under it.
    let not_black = |model: &UiModel<Field>, route: &str| {
        let rows = text_of(model);
        // The body: every row between the top bar and the compose line.
        let body = &rows[1..rows.len() - 1];
        let filled = body.iter().filter(|r| !r.trim().is_empty()).count();
        assert!(
            body.iter().any(|r| r.contains("Hier Lew")),
            "{route}: the texts under the first bubble still show: {rows:#?}"
        );
        assert!(
            body.iter()
                .position(|r| r.contains(screen::BAR))
                .is_some_and(|at| at < body.len() / 2),
            "{route}: the first bubble at the top, its bar in the flow: {rows:#?}"
        );
        assert!(
            filled * 2 >= body.len(),
            "{route}: the pane draws the rows the view covers, not a black body: {rows:#?}"
        );
    };
    let open = |model: &mut UiModel<Field>| {
        model.focus = Pane::Conversations;
        model.chat_anchor = None;
        on_the_board(model, M9Key::Enter);
        assert_eq!(model.focus, Pane::Messages);
        let opened = text_of(model);
        assert!(
            opened.iter().any(|r| r.contains("der neueste")),
            "{opened:#?}"
        );
    };
    // The image the walk ran (a825cf3b, before 1529): the ring's up went to
    // the model a message a press and the chat had no anchor, so the pane
    // stood where the cursor's bubble alone puts it. A cursor moved without
    // the M9's lines still draws that way.
    open(&mut model);
    for _ in 0..30 {
        press(&mut model, M9Key::Up);
    }
    assert!(model.chat_anchor.is_none(), "this route leaves no anchor");
    not_black(&model, "a message a press, no anchor");
    // The board today: the walk's three holds, ten answers of the ring's
    // up each, every answer a line (1529).
    open(&mut model);
    for _ in 0..30 {
        let Read::Code(code) = MEASURED.read(&[0xb5; 8]) else {
            panic!("a ring answer is a code");
        };
        let event = Key::of(code).and_then(Key::event).expect("the ring's up");
        let mut row = 0;
        loquax_m9::group::press(&mut model, &mut row, &event);
    }
    not_black(&model, "held on the board");
    check("m9-chat-top-held.png", &shown(&model));
}

/// Slice two: the compose line typed into with the M9's keys. `i` writes,
/// the letters go into the field, Backspace erases, Left moves the caret,
/// Enter sends what the field holds and Back leaves the line with its text
/// kept as the chat's draft. The frame with the caret is pinned, and so is
/// the chat with the sent message on its sending rung.
#[test]
fn typing_with_the_keys_sends_from_the_compose_line() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    press(&mut model, M9Key::Enter);
    press(&mut model, M9Key::Char('i'));
    assert_eq!(model.focus, Pane::Compose);
    type_in(&mut model, "Bis gleich in Köln!x");
    press(&mut model, M9Key::Backspace);
    press(&mut model, M9Key::Left);
    press(&mut model, M9Key::Left);
    assert_eq!(
        (model.compose.value(), model.compose.cursor()),
        ("Bis gleich in Köln!", 17)
    );
    let rows = text_of(&model);
    assert_eq!(
        rows.last().map(|r| r.trim_end().to_string()).as_deref(),
        Some(" Bis gleich in Köln!"),
        "the compose line holds the text"
    );
    check("m9-compose.png", &shown(&model));

    let peer = model.conversations[model.selected].peer;
    let effects = press(&mut model, M9Key::Enter);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            UiEffect::Send { to, body, .. } if *to == peer && body == "Bis gleich in Köln!"
        )),
        "Enter sends the field: {effects:?}"
    );
    assert_eq!(
        model.compose.value(),
        "",
        "the field is empty after the send"
    );
    assert_eq!(model.focus, Pane::Compose, "and keeps the keys");
    let rows = text_of(&model);
    let sent = rows
        .iter()
        .rev()
        .find(|r| r.contains("Bis gleich in Köln!"))
        .unwrap_or_else(|| panic!("the sent message is drawn: {rows:#?}"));
    assert!(
        sent.trim_end().ends_with(" ◷"),
        "on its sending rung: {sent:?}"
    );
    check("m9-sent.png", &shown(&model));

    // Back cancels the line: the keys go to the bubbles, the typed text
    // stays the chat's draft and shows in the line, no caret.
    type_in(&mut model, "Entwurf");
    press(&mut model, M9Key::Back);
    assert_eq!(model.focus, Pane::Messages);
    assert_eq!(model.conversations[model.selected].draft.text, "Entwurf");
    let rows = text_of(&model);
    assert_eq!(
        rows.last().map(|r| r.trim_end().to_string()).as_deref(),
        Some(" Entwurf")
    );
}

/// Slice three (1359), on the protocol the board measured (1427): a walk
/// of controller answers, through the measured read, the driver, the key
/// table and the model, opens the chat, types "hi&0" and sends it. Every
/// code in it is one the bench capture saw.
#[test]
fn a_recorded_walk_of_key_codes_types_and_sends() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    let recording = include_str!("keyboard/walk.txt");
    let mut effects = Vec::new();
    for answer in kbraw::parse_raw(recording) {
        let (_, bytes) = answer.expect("a recorded answer");
        let Read::Code(code) = MEASURED.read(&bytes) else {
            continue;
        };
        if let Some(e) = Key::of(code).and_then(Key::event) {
            effects.extend(update(&mut model, UiEvent::Key(e)));
        }
    }
    let peer = model.conversations[model.selected].peer;
    assert!(
        effects.iter().any(|e| matches!(
            e,
            UiEffect::Send { to, body, .. } if *to == peer && body == "hi&0"
        )),
        "the walk sends hi&0: {effects:?}"
    );
}

const ME: [u8; 16] = [0x01; 16];
const LEW: [u8; 16] = [0x02; 16];
const ANNA: [u8; 16] = [0x03; 16];
const BOB: [u8; 16] = [0x04; 16];
const GROUP: [u8; 16] = [0x0f; 16];
/// The group's day, 2027-01-15 08:00 UTC.
const T0: i64 = 1_800_000_000;

fn clock(model: &mut UiModel<Field>, secs: i64) {
    update(
        model,
        UiEvent::Clock(WallTime {
            unix_secs: T0 + secs,
            offset_secs: 3600,
        }),
    );
}

fn arrive(model: &mut UiModel<Field>, from: [u8; 16], id: u8, body: &str, lev: Lev) {
    let mut fields = Vec::new();
    lev.put_into(&mut fields);
    update(
        model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [id; 32],
            source: from,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields,
            verified: true,
        }),
    );
}

fn members(epoch: u64, list: &[([u8; 16], &str, u64)]) -> Lev {
    Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        members: Some(Members {
            epoch,
            members: list
                .iter()
                .map(|(address, name, flags)| Member {
                    address: *address,
                    name: (*name).into(),
                    flags: *flags,
                })
                .collect(),
            ended: false,
        }),
        ..Lev::default()
    }
}

fn copy_at(writer: [u8; 16], sent_ms: u64) -> Lev {
    Lev {
        capabilities: Some(CAP_GROUPS),
        group_copy: Some(GroupCopy {
            sender: writer,
            sent_ms,
            epoch: 1,
        }),
        ..Lev::default()
    }
}

/// A group's chat with everything a group draws (1307): Lew's invitation
/// and the list, a digest of Anna's and Bob's lines under their names,
/// Anna's next line, a line of Bob's from before it that came late, Bob
/// leaving, and our own line on its sending rung.
fn group_model() -> UiModel<Field> {
    let mut model = UiModel::<Field>::new(&[LEW]);
    model.address = Some(ME);
    clock(&mut model, 0);
    let invite = Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        invite: Some(Invite {
            group: GROUP,
            public_key: [7; 64],
            name: "Familie".into(),
            admin_name: "Lew".into(),
            epoch: 1,
        }),
        ..Lev::default()
    };
    arrive(&mut model, LEW, 1, "invite", invite);
    let joined = MEMBER_JOINED;
    let list = [
        (LEW, "Lew", MEMBER_ADMIN | joined),
        (ME, "Me", joined),
        (ANNA, "Anna", joined),
        (BOB, "Bob", joined),
    ];
    arrive(&mut model, GROUP, 2, "", members(1, &list));
    update(&mut model, UiEvent::Open { peer: GROUP });
    let t = (T0 as u64) * 1000;
    clock(&mut model, 60);
    let digest = Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        digest: Some(
            [
                (ANNA, t, "Brot ist alle"),
                (BOB, t + 2_000, "Ich bring welches mit 🍞"),
            ]
            .iter()
            .map(|(sender, sent_ms, text)| DigestLine {
                sender: *sender,
                sent_ms: *sent_ms,
                text: (*text).into(),
                left_out: None,
            })
            .collect(),
        ),
        ..Lev::default()
    };
    arrive(
        &mut model,
        GROUP,
        20,
        "Anna: Brot ist alle\nBob: Ich bring welches mit 🍞",
        digest,
    );
    clock(&mut model, 120);
    arrive(
        &mut model,
        GROUP,
        21,
        "Anna: Bis gleich",
        copy_at(ANNA, t + 9_000),
    );
    clock(&mut model, 180);
    arrive(
        &mut model,
        GROUP,
        22,
        "Bob: Wer kommt noch?",
        copy_at(BOB, t + 5_000),
    );
    clock(&mut model, 240);
    let mut left = list;
    left[3].2 = MEMBER_LEFT;
    arrive(&mut model, GROUP, 23, "", members(2, &left));
    clock(&mut model, 300);
    update(
        &mut model,
        UiEvent::Write {
            peer: GROUP,
            body: "Ich komme um acht".into(),
        },
    );
    clock(&mut model, 360);
    model.focus = Pane::Messages;
    model
}

fn group_look(model: &UiModel<Field>) -> Look {
    Look {
        cursor: None,
        ..Look::of(model)
    }
}

mod group_ticks {
    include!("../../loquax/tests/support/group_ticks.rs");
}

/// 1466, the group's ticks tell the truth too: the M9 draws the group's
/// chat of `scene-group-ticks.json`, our line every member's device has on
/// the two-tick rung (`vv`) and the one Ben's device has not said yet on
/// the one-tick rung (`v `). The control: the frame with the second line
/// at two ticks differs.
#[test]
fn a_groups_one_and_two_ticks_are_the_golden_frame() {
    let mut model = UiModel::<Field>::new(&[]);
    model.address = Some([0x30; 16]);
    group_ticks::group_ticks(&mut model, 1_800_000_000, 3600);
    model.focus = Pane::Messages;
    let scene = model.scene(LayoutClass::Compact);
    let look = group_look(&model);
    let rows: Vec<String> = screen::lines(&scene, &look, COLS, ROWS)
        .iter()
        .map(row_text)
        .collect();
    let row = |words: &str| {
        rows.iter()
            .rfind(|r| r.contains(words))
            .unwrap_or_else(|| panic!("{words:?} is not on the screen: {rows:#?}"))
            .trim_end()
            .to_string()
    };
    // The atlas's glyphs, never the ASCII rung (1447).
    assert!(row("Brot ist da").ends_with("✓✓"), "{rows:#?}");
    let one = row("Wer kommt mit?");
    assert!(one.ends_with('✓') && !one.ends_with("✓✓"), "{rows:#?}");
    let frame = draw_in(&scene, &look);
    check("m9-group-ticks.png", &frame);

    let mut both = scene.clone();
    for node in both
        .chat
        .as_mut()
        .expect("the chat")
        .groups
        .iter_mut()
        .flat_map(|g| g.bubbles.iter_mut())
    {
        if let Some(mark) = node.mark.as_mut() {
            mark.mark = loquax::ui::chat::Mark::Delivered;
        }
        node.bubble.mark = Some(loquax::ui::chat::Mark::Delivered);
    }
    assert!(
        changed_rows(&frame, &draw_in(&both, &look)).is_some(),
        "the one tick is in the frame"
    );
}

/// Slice two: a group's chat. The writers' names over their bubbles, the
/// digest's lines a bubble each, the late line saying so under it, Bob
/// leaving a centred note, ours flush right on its rung. The positive
/// control takes the late mark and the names away: each changes the frame.
#[test]
fn a_group_chat_is_the_golden_frame() {
    let model = group_model();
    let scene = model.scene(LayoutClass::Compact);
    let look = group_look(&model);
    let lines = screen::lines(&scene, &look, COLS, ROWS);
    let rows: Vec<String> = lines.iter().map(row_text).collect();
    let has = |words: &str| rows.iter().any(|r| r.contains(words));
    // The last screen, twelve rows: the newest lines from the bottom up.
    for words in [
        "Anna",
        "Bob",
        "Bis gleich",
        "Wer kommt noch?",
        "arrived late",
        "Ich komme um acht",
    ] {
        assert!(has(words), "{words:?} is not on the screen: {rows:#?}");
    }
    // The digest's lines stand above it, where the cursor scrolls: they
    // are not on the last screen, and with the cursor on the digest, or
    // on Anna's next line (the digest's lines share one message's key, so
    // the cursor stands on its first), each is.
    for (words, body) in [
        ("Brot ist alle", "Brot ist alle"),
        ("Ich bring welches mit ?", "Bis gleich"),
    ] {
        let key = scene
            .chat
            .as_ref()
            .into_iter()
            .flat_map(|chat| &chat.groups)
            .flat_map(|group| &group.bubbles)
            .find(|node| node.bubble.body.contains(body))
            .map(|node| node.bubble.key);
        assert!(key.is_some(), "{body:?} is a bubble");
        let up = Look {
            cursor: key,
            ..look
        };
        let above: Vec<String> = screen::lines(&scene, &up, COLS, ROWS)
            .iter()
            .map(row_text)
            .collect();
        assert!(!has(words), "{words:?} fits the last screen too: {rows:#?}");
        assert!(
            above.iter().any(|r| r.contains(words)),
            "{words:?} is not on the screen at its cursor: {above:#?}"
        );
    }
    let note = rows
        .iter()
        .find(|r| r.contains("── ") && r.contains("Bob") && !r.contains("Today"))
        .unwrap_or_else(|| panic!("Bob leaving is a note line: {rows:#?}"));
    assert!(note.starts_with(' '), "centred: {note:?}");
    let frame = draw_in(&scene, &look);
    check("m9-group.png", &frame);

    let mut on_time = scene.clone();
    let chat = on_time.chat.as_mut().expect("the chat");
    let late: usize = chat
        .groups
        .iter_mut()
        .flat_map(|g| g.bubbles.iter_mut())
        .filter(|b| b.bubble.late)
        .map(|b| b.bubble.late = false)
        .count();
    assert_eq!(late, 1, "one line came late");
    assert!(
        changed_rows(&frame, &draw_in(&on_time, &look)).is_some(),
        "the late mark is in the frame"
    );
    let mut unnamed = scene.clone();
    for group in &mut unnamed.chat.as_mut().expect("the chat").groups {
        group.named = false;
    }
    assert!(
        changed_rows(&frame, &draw_in(&unnamed, &look)).is_some(),
        "the writers' names are in the frame"
    );
}

/// The redraw rule of slice one on the chat: one renderer moving the
/// cursor over the bubbles, and then typing into the line, gives the
/// frames a fresh renderer draws.
#[test]
fn a_chat_redraw_is_the_frame_a_fresh_draw_gives() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    press(&mut model, M9Key::Enter);
    let mut frame = Frame::new();
    {
        let mut renderer = Renderer::new(&mut frame).expect("renderer");
        let mut redraw = |model: &UiModel<Field>| {
            renderer
                .draw(&screen::scene(model), &Look::of(model))
                .expect("draw");
        };
        redraw(&model);
        for _ in 0..9 {
            press(&mut model, M9Key::Up);
            redraw(&model);
        }
        press(&mut model, M9Key::Char('i'));
        redraw(&model);
        for c in "Grüße 😀 zurück".chars() {
            press(&mut model, M9Key::Char(c));
            redraw(&model);
        }
        press(&mut model, M9Key::Left);
        redraw(&model);
    }
    let fresh = shown(&model);
    assert!(
        frame.pixels() == fresh.pixels(),
        "the redraw differs in pixel rows {:?}",
        changed_rows(&frame, &fresh)
    );
}
