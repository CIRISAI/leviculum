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
use loquax::home::board::{Medium, Way, Word};
use loquax::outbox::OutboxEvent;
use loquax::store::Store;
use loquax::ui::scene::{LayoutClass, Scene};
use loquax::ui::strings::Lang;
use loquax::ui::{update, Pane, UiEffect, UiEvent, UiModel, NONE_OPEN};
use loquax::when::WallTime;
use loquax_m9::field::Field;
use loquax_m9::font::Size;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::kbraw::{self, Read, MEASURED};
use loquax_m9::keyboard::Key;
use loquax_m9::keys::{event, M9Key};
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look, ENTRY_ROWS};

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
    // The board's screen (1532): its rows, no tab bar.
    model.viewing = screen::viewing(Size::DEFAULT);
    model
}

/// The chats list at the M9's class, nothing open.
///
/// The scene carries every row (1595): the board builds only the rows on
/// its screen, and [`with_cursor`] moves the cursor through rows the
/// scene must already hold. A list viewed whole, not apart, draws the
/// frame the board's list draws (no top kept: the cursor's own lowest
/// line, as a list apart with nothing selected), so the goldens stand.
fn list_scene() -> Scene {
    let home = tempfile::tempdir().expect("home");
    let mut model = ugly_model(home.path());
    model.selected = NONE_OPEN;
    model.viewing = loquax::ui::Viewing {
        list_rows: loquax::ui::Viewing::ALL.list_rows,
        apart: false,
        ..model.viewing
    };
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
    draw_sized(scene, look, Size::DEFAULT)
}

/// `scene` drawn in `look` and type `size` (1532).
fn draw_sized(scene: &Scene, look: &Look, size: Size) -> Frame {
    let mut frame = Frame::new();
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    renderer.set_size(size);
    renderer.draw(scene, look).expect("draw");
    drop(renderer);
    frame
}

/// What `model` shows on the M9, drawn fresh: its scene in its look, in
/// the size the model is viewed in.
fn shown(model: &UiModel<Field>) -> Frame {
    draw_sized(
        &screen::scene(model),
        &Look::of(model),
        screen::size_of(model),
    )
}

/// The grid `model` is viewed in.
fn grid(model: &UiModel<Field>) -> (usize, usize) {
    screen::size_of(model).grid()
}

/// The screen's characters, a string a row, as the cells hold them.
fn text_of(model: &UiModel<Field>) -> Vec<String> {
    let (cols, rows) = grid(model);
    let lines = screen::lines(&screen::scene(model), &Look::of(model), cols, rows);
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

/// 1452: two peers called Lew collide, so both carry their hash tail on
/// the list, the rows the name alone, and the open one says "Zwei
/// Kontakte heißen Lew" on a row of its own under the bar (1587);
/// Anna, whose name nobody shares, is her name alone, and nobody is hex.
#[test]
fn a_shared_name_is_the_golden_frame() {
    let (lew, other, anna) = ([0x1e; 16], [0xe1; 16], [0xa0; 16]);
    let mut model = UiModel::<Field>::new(&[lew, other, anna]);
    model.lang = Lang::De;
    model.address = Some([OWN as u8; 16]);
    update(&mut model, UiEvent::Clock(now()));
    for (peer, name) in [(lew, "Lew"), (other, "lew"), (anna, "Anna")] {
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Announced {
                destination: peer,
                name: Some(name.to_string()),
                path_response: false,
            }),
        );
    }
    model.toasts.clear();
    model.viewing = screen::viewing(Size::DEFAULT);
    model.selected = NONE_OPEN;
    let list = text_of(&model).join("\n");
    assert!(list.contains("Lew ·1e1e"), "{list}");
    assert!(list.contains("lew ·e1e1"), "{list}");
    assert!(list.contains("Anna"), "{list}");
    assert!(!list.contains("Anna ·"), "{list}");
    check("m9-same-name-list.png", &shown(&model));
    model.selected = 0;
    model.focus = Pane::Conversations;
    press(&mut model, M9Key::Enter);
    assert_eq!(model.focus, Pane::Messages);
    // 1587: the bar says the name with its tail, the sentence its own row
    // under it, whole, not cut at the bar's 35 cells.
    let rows = text_of(&model);
    let chat = rows.join("\n");
    assert!(rows[0].contains("Lew ·1e1e"), "{chat}");
    assert!(!rows[0].contains("Zwei"), "{chat}");
    assert_eq!(rows[1].trim(), "Zwei Kontakte heißen Lew", "{chat}");
    assert!(!chat.contains('…'), "{chat}");
    check("m9-same-name-chat.png", &shown(&model));
    // The control: Anna's chat has no collision and no such row, its
    // first row under the bar is the chat's own.
    model.selected = 2;
    model.focus = Pane::Conversations;
    press(&mut model, M9Key::Enter);
    let anna = text_of(&model);
    assert!(anna[0].contains("Anna"), "{anna:#?}");
    assert!(!anna.join("\n").contains("Kontakte"), "{anna:#?}");
}

/// 1532: the list and the open chat in the large size, the 10 x 20 of
/// 1428, which the reader switches to; the small one is the default the
/// goldens above draw. The control: the two sizes draw different frames
/// and the small one shows more entries (7 against 5).
#[test]
fn the_list_and_the_chat_in_the_large_size_are_golden_frames() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    model.viewing = screen::viewing(Size::Large);
    assert_eq!(screen::size_of(&model), Size::Large);
    let large = shown(&model);
    check("m9-chats-large.png", &large);
    let entries = |model: &UiModel<Field>| {
        screen::scene(model)
            .list
            .map_or(0, |list| list.rows.len().min(model.viewing.list_rows))
    };
    assert_eq!(entries(&model), 5);
    let mut small = model.clone();
    small.viewing = screen::viewing(Size::Small);
    assert_eq!(entries(&small), 7);
    assert!(changed_rows(&large, &shown(&small)).is_some());
    // No tab bar in either: the last row is an entry's, not "Chats".
    for model in [&model, &small] {
        let rows = text_of(model);
        let last = rows.last().expect("a row");
        assert!(!last.contains("Chats"), "{rows:#?}");
        assert_eq!(rows.len(), grid(model).1);
    }
    press(&mut model, M9Key::Enter);
    assert_eq!(model.focus, Pane::Messages);
    check("m9-chat-large.png", &shown(&model));
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
    let cell = usize::from(Size::DEFAULT.cell_px().1);
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
    update(
        &mut model,
        UiEvent::Viewing(screen::viewing(screen::Size::DEFAULT)),
    );
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
    update(
        &mut model,
        UiEvent::Viewing(screen::viewing(screen::Size::DEFAULT)),
    );
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
    let rows: Vec<String> = screen::lines(&scene, &look, grid(&model).0, grid(&model).1)
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
    // The large size (1532): the screen whose rows this test counts, the
    // digest's lines above its last screen; the small one holds them.
    let mut model = group_model();
    model.viewing = screen::viewing(Size::Large);
    let scene = model.scene(LayoutClass::Compact);
    let look = group_look(&model);
    let lines = screen::lines(&scene, &look, grid(&model).0, grid(&model).1);
    let rows: Vec<String> = lines.iter().map(row_text).collect();
    let has = |words: &str| rows.iter().any(|r| r.contains(words));
    // The last screen, twelve rows in the large size: the newest lines from the bottom up.
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
        let above: Vec<String> = screen::lines(&scene, &up, grid(&model).0, grid(&model).1)
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
    let frame = draw_sized(&scene, &look, Size::Large);
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
        changed_rows(&frame, &draw_sized(&on_time, &look, Size::Large)).is_some(),
        "the late mark is in the frame"
    );
    let mut unnamed = scene.clone();
    for group in &mut unnamed.chat.as_mut().expect("the chat").groups {
        group.named = false;
    }
    assert!(
        changed_rows(&frame, &draw_sized(&unnamed, &look, Size::Large)).is_some(),
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

mod late_message {
    include!("../../loquax/tests/support/late_message.rs");
}

/// 1505, a late message sits at its sent time: the M9 draws Anna's chat of
/// the shared fixture, the message she wrote last night at 22:10 that came
/// today at 14:05 second, under last night's day, today's two below.
#[test]
fn a_late_message_is_the_golden_frame() {
    let mut model = UiModel::<Field>::new(&[]);
    model.address = Some([0x30; 16]);
    late_message::late_message(&mut model, 1_799_971_200, 3600);
    model.focus = Pane::Messages;
    model.toasts.clear();
    let scene = model.scene(LayoutClass::Compact);
    let look = group_look(&model);
    let rows: Vec<String> = screen::lines(&scene, &look, grid(&model).0, grid(&model).1)
        .iter()
        .map(row_text)
        .collect();
    let row = |words: &str| {
        rows.iter()
            .rposition(|r| r.contains(words))
            .unwrap_or_else(|| panic!("{words:?} is not on the screen: {rows:#?}"))
    };
    assert!(
        row("Gute Nacht") < row("Kommst du morgen mit?"),
        "{rows:#?}"
    );
    assert!(
        row("Kommst du morgen mit?") < row("Bis gleich am Bahnhof"),
        "{rows:#?}"
    );
    assert!(
        row("Bis gleich am Bahnhof") < row("Bin unterwegs"),
        "{rows:#?}"
    );
    check("m9-late-message.png", &draw_in(&scene, &look));
}

/// 1595, window 20's hang.walk: a Back lost in a FIFO overrun left the
/// board in the chat, and every later Back "changed nothing". In a chat
/// the shared key table's Enter opens the message's detail view, and Back
/// leaves what it is in, the detail first. 1595 pinned that as one level
/// a Back leaves; but the M9 draws no detail view (the scene has none, and
/// its `[PAGE]` line said `chat`), so the level was one nobody saw, and
/// window 23 stood five minutes on it. Since 1612 the M9 closes a state it
/// does not draw at once (`group::close_unshown`, `tests/unshown.rs`):
/// Enter in the chat leaves the chat on the screen as it was, and one
/// Back leaves it. The M9's rule (UI decisions 2026-11-07) holds over
/// what the screen shows: each Back leaves one level of it.
#[test]
fn back_leaves_the_chat_after_an_enter_in_it() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    let mut row = 0;
    let place = |model: &UiModel<Field>| loquax_m9::group::Place::of(model);
    loquax_m9::group::press(&mut model, &mut row, &event(M9Key::Enter));
    assert_eq!(
        place(&model),
        loquax_m9::group::Place::Chat,
        "Enter opens the chat"
    );
    assert!(model.detail.is_none());
    loquax_m9::group::press(&mut model, &mut row, &event(M9Key::Enter));
    assert!(
        model.detail.is_none(),
        "Enter in the chat leaves no detail the M9 does not draw"
    );
    assert_eq!(place(&model), loquax_m9::group::Place::Chat);
    loquax_m9::group::press(&mut model, &mut row, &event(M9Key::Back));
    assert_eq!(
        place(&model),
        loquax_m9::group::Place::List,
        "one Back leaves the chat"
    );
}

mod reply_scene {
    include!("../../loquax/tests/support/reply_scene.rs");
}

/// The M9 on Anna's chat of the shared fixture (1451): her question, her
/// photo, our reply quoting her question, her thumbs up on our reply, her
/// last line; the clock at 09:30, the chat open with the keys on its
/// bubbles, the board's screen and size.
fn reply_model() -> UiModel<Field> {
    let mut model = UiModel::<Field>::new(&[]);
    model.address = Some([0x30; 16]);
    model.viewing = screen::viewing(Size::DEFAULT);
    model.selected = reply_scene::reply_scene(&mut model, 1_799_971_200, 3600);
    model.focus = Pane::Messages;
    model.toasts.clear();
    model
}

/// The row of `rows` that holds `words`, the lowest.
fn row_with(rows: &[String], words: &str) -> usize {
    rows.iter()
        .rposition(|r| r.contains(words))
        .unwrap_or_else(|| panic!("{words:?} is not on the screen: {rows:#?}"))
}

/// 1451, the walk's blind spots, the reply: `m9-reply.png` is Anna's chat
/// as the M9 opens it, the cursor on her newest line. Our reply carries
/// its quote as the bubble's first line, `> Kommst du morgen mit zum
/// Markt?` (`chat::block` draws `> ` and the quote's first line, in
/// italics), above our text, in our bubble on the right.
/// Her photo stands in the same frame as its words, `[#] Photo, 185 KB`,
/// under her question; `m9-photo.png` pins the frame with the cursor on
/// it (two presses up), which differs only in the selection bar.
#[test]
fn a_reply_and_a_photo_are_the_golden_frames() {
    let mut model = reply_model();
    let rows = text_of(&model);
    let quote = row_with(&rows, "> Kommst du morgen mit zum Markt?");
    let reply = row_with(&rows, "Ja, um zehn am Brunnen");
    assert_eq!(quote + 1, reply, "the quote heads our bubble: {rows:#?}");
    let photo = row_with(&rows, "[#] Photo, 185 KB");
    assert!(
        row_with(&rows, "Kommst du morgen mit zum Markt?  ") < photo && photo < quote,
        "her question, her photo, our reply: {rows:#?}"
    );
    assert!(rows[row_with(&rows, "Super, bis morgen")].starts_with('▌'));
    check("m9-reply.png", &shown(&model));

    press(&mut model, M9Key::Up);
    press(&mut model, M9Key::Up);
    let rows = text_of(&model);
    assert!(
        rows[row_with(&rows, "[#] Photo, 185 KB")].starts_with('▌'),
        "the cursor on the photo: {rows:#?}"
    );
    check("m9-photo.png", &shown(&model));
}

/// 1451, the reaction: `m9-reaction.png` is the chat with the cursor on
/// our reply, the bubble Anna put her thumbs up on. The M9 draws no
/// reaction: `chat::block` lays the quote, the media, the text and the
/// time with its mark, and never reads the bubble's chips, so the thumbs
/// up is nowhere on the screen (a finding of the walk, not a choice).
/// The frame is pinned as it is; the assertion below says what it shows.
#[test]
fn a_reaction_is_not_drawn_on_the_m9() {
    let mut model = reply_model();
    press(&mut model, M9Key::Up);
    let rows = text_of(&model);
    let reply = row_with(&rows, "Ja, um zehn am Brunnen");
    assert!(rows[reply].starts_with('▌'), "{rows:#?}");
    assert!(
        rows.iter()
            .all(|r| !r.contains('\u{1F44D}') && !r.contains("+1")),
        "the M9 draws no reaction chip: {rows:#?}"
    );
    check("m9-reaction.png", &shown(&model));
}

/// 1451, a reply being written: `m9-reply-compose.png` is the chat after
/// `reply_composing` (the draft "Ich bring Kaffee mit" answering Anna's
/// last line) and `i`, which gives the compose line the keys, the caret
/// after the text. The M9 shows the text alone: its compose line
/// (`chat::compose_line`) reads the scene's `Compose::text` and never its
/// `reply`, so nothing on the screen says which line the reply answers.
#[test]
fn a_reply_being_written_is_the_golden_frame() {
    let mut model = reply_model();
    reply_scene::reply_composing(&mut model);
    press(&mut model, M9Key::Char('i'));
    assert_eq!(model.focus, Pane::Compose);
    assert_eq!(model.compose.value(), reply_scene::REPLY_DRAFT);
    let scene = screen::scene(&model);
    let compose = &scene.chat.as_ref().expect("the chat").compose;
    assert_eq!(compose.reply, Some(reply_scene::REPLY_LAST));
    let rows = text_of(&model);
    assert_eq!(
        rows.last().map(|r| r.trim_end().to_string()).as_deref(),
        Some(" Ich bring Kaffee mit"),
        "the compose line holds the draft and nothing of the reply: {rows:#?}"
    );
    check("m9-reply-compose.png", &shown(&model));
}

/// The house key's settings page (1532, 1561) drawn as the board draws
/// it (`Renderer::text` over `Overview::lines` of `house::page`), open,
/// in `size`, in `lang`, the size row's value stepped to `stepped` or not.
fn overview_frame(lang: Lang, size: Size, stepped: Option<Size>) -> (Frame, Vec<String>) {
    let mut overview = loquax_m9::house::Overview::new(size);
    overview.open = true;
    overview.stepped = stepped.map(Size::index);
    let mut model = UiModel::<Field>::new(&[]);
    model.lang = lang;
    let page = loquax_m9::house::page(&model, size, "0.0.0");
    let look = Look::m9(lang, Some(now()));
    let (cols, rows) = size.grid();
    let words = overview
        .lines(&page, &look, cols, rows)
        .iter()
        .map(row_text)
        .collect();
    let mut frame = Frame::new();
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    renderer.set_size(size);
    renderer
        .text(|cols, rows| overview.lines(&page, &look, cols, rows))
        .expect("draw");
    drop(renderer);
    (frame, words)
}

/// 1451, 1561, the M9's settings: the house key's page (`house.rs`), the
/// core's settings for the M9 (`settings::PLACES`' M9 column):
/// Appearance with the type size row, Advanced with the version and the
/// map's data. `m9-settings.png` is that page in English,
/// `m9-settings-de.png` in German: its words go through the catalogue.
/// `m9-settings-tiny.png` is it in the very small size with the value
/// stepped (between `<` and `>`, the hint says what Enter does).
#[test]
fn the_house_keys_page_is_the_settings_golden() {
    let (en, rows) = overview_frame(Lang::En, Size::DEFAULT, None);
    assert!(rows[0].starts_with(" Settings"), "{rows:#?}");
    assert!(rows[2].starts_with(" Appearance"), "{rows:#?}");
    assert!(rows[3].starts_with("▌ Text"), "{rows:#?}");
    assert!(rows[3].trim_end().ends_with("small"), "{rows:#?}");
    assert!(rows[5].starts_with(" Advanced"), "{rows:#?}");
    assert!(
        rows[6].contains("Version") && rows[6].contains("0.0.0"),
        "{rows:#?}"
    );
    check("m9-settings.png", &en);
    let (de, rows) = overview_frame(Lang::De, Size::DEFAULT, None);
    assert!(rows[0].starts_with(" Einstellungen"), "{rows:#?}");
    assert!(rows[3].trim_end().ends_with("klein"), "{rows:#?}");
    check("m9-settings-de.png", &de);
    assert_ne!(en.pixels(), de.pixels());
    let (tiny, rows) = overview_frame(Lang::De, Size::Tiny, Some(Size::Large));
    assert_eq!(rows.len(), 20);
    assert!(rows[3].trim_end().ends_with("< groß >"), "{rows:#?}");
    assert!(
        rows.iter().any(|r| r.contains("Enter übernimmt")),
        "{rows:#?}"
    );
    check("m9-settings-tiny.png", &tiny);
}

/// 1561: the list and the open chat in the very small size, the 6 x 12,
/// which a reader may choose. The control: it shows more entries than
/// the small size (9 against 7) and draws a different frame.
#[test]
fn the_list_and_the_chat_in_the_tiny_size_are_golden_frames() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    model.viewing = screen::viewing(Size::Tiny);
    assert_eq!(screen::size_of(&model), Size::Tiny);
    let tiny = shown(&model);
    check("m9-chats-tiny.png", &tiny);
    let entries = |model: &UiModel<Field>| {
        screen::scene(model)
            .list
            .map_or(0, |list| list.rows.len().min(model.viewing.list_rows))
    };
    let mut small = model.clone();
    small.viewing = screen::viewing(Size::Small);
    assert_eq!(entries(&small), 7);
    assert_eq!(entries(&model), 9);
    assert!(changed_rows(&tiny, &shown(&small)).is_some());
    let rows = text_of(&model);
    assert_eq!(rows.len(), 20);
    assert_eq!(rows[0].chars().count(), 53);
    press(&mut model, M9Key::Enter);
    assert_eq!(model.focus, Pane::Messages);
    check("m9-chat-tiny.png", &shown(&model));
}

/// The board's chats list (the fixture's, in English) with `word` at the
/// top bar's right end, as the board's look carries the home's word.
fn list_with_word(word: Option<Word>) -> (Frame, String) {
    list_with_way(word, None)
}

/// [`list_with_word`] with the board's way to its home (1478).
fn list_with_way(word: Option<Word>, way: Option<Way>) -> (Frame, String) {
    let mut model = loquax_m9::fixture::model();
    model.lang = Lang::En;
    let look = Look {
        home: word,
        way,
        ..Look::of(&model)
    };
    let scene = screen::scene(&model);
    let (cols, rows) = grid(&model);
    let bar = row_text(&screen::lines(&scene, &look, cols, rows)[0]);
    (draw_sized(&scene, &look, screen::size_of(&model)), bar)
}

/// 1451, the home's words in the top bar (1415, `loquax::home::board::
/// Word`), which no golden showed before. `m9-joining.png`: the pairing
/// screen with a whole code typed and Enter pressed, the join running,
/// its bar "Joining your mailbox" where it said "Link this M9 to your
/// mailbox" (the board keeps the pairing on the screen until the home
/// takes the code). `m9-offline.png`: the chats list while the mailbox is
/// out of reach, "Disconnected" at the bar's right end (1564: one short
/// word; "Mailbox unreachable" before). `m9-online.png`: the return,
/// "Connected" there, where 1451 drew the catalogue's sentence ("You.
/// Your mailbox is connected.") cut by the bar.
#[test]
fn the_homes_words_in_the_top_bar_are_golden_frames() {
    use loquax::home::code::HomeCode;
    use loquax_m9::pair::{self, Pairing};
    let code = HomeCode {
        public_key: core::array::from_fn(|i| i as u8),
        token: [7; 16],
    };
    let mut pairing = Pairing::new();
    let mut got = None;
    for c in code.encode().chars() {
        got = got.or(pairing.key(&event(M9Key::Char(c))));
    }
    got = got.or(pairing.key(&event(M9Key::Enter)));
    assert_eq!(got, Some(code), "the code is taken");
    let model = loquax_m9::fixture::model();
    let look = Look {
        row: 0,
        home: Some(Word::Joining),
        lang: Lang::En,
        ..Look::of(&model)
    };
    let (cols, rows) = Size::DEFAULT.grid();
    let lines = pair::screen(&pairing, Some(Word::Joining), &look, cols, rows);
    assert!(
        row_text(&lines[0]).contains("Joining your mailbox"),
        "{:?}",
        row_text(&lines[0])
    );
    let mut joining = Frame::new();
    let mut renderer = Renderer::new(&mut joining).expect("renderer");
    pair::draw(&mut renderer, &pairing, Some(Word::Joining), &look).expect("draw");
    drop(renderer);
    check("m9-joining.png", &joining);

    let (none, plain) = list_with_word(None);
    // Out of reach with no path held: since 1478 the bar says there is
    // no way, where it said "Disconnected".
    let (offline, bar) = list_with_word(Some(Word::Unreachable));
    assert!(
        bar.trim_end().ends_with("No way to mailbox"),
        "{bar:?} against {plain:?}"
    );
    check("m9-offline.png", &offline);
    // `m9-way-radio.png` (1478): connected over a radio path of two hops,
    // in the short form: the long one does not fit beside "Chats" in the
    // default type's 35 cells.
    let (way, bar) = list_with_way(
        Some(Word::Connected),
        Some(Way {
            medium: Medium::Radio,
            hops: 2,
        }),
    );
    assert!(bar.trim_end().ends_with(" Radio · 2 stations"), "{bar:?}");
    check("m9-way-radio.png", &way);
    let (online, bar) = list_with_word(Some(Word::Connected));
    assert!(bar.trim_end().ends_with(" Connected"), "{bar:?}");
    assert!(!bar.contains("You"), "{bar:?}");
    check("m9-online.png", &online);
    assert_ne!(none.pixels(), offline.pixels());
    assert_ne!(offline.pixels(), online.pixels());
}
