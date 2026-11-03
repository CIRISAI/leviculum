//! The M9's screens drawn on the host (1351, 1356, `handheld-messenger.md`,
//! "The test plan", item 1): the ugly store's model (1173), its Compact
//! scene, drawn by the M9's renderer through mousefood into the 320 x 240
//! RGB565 frame, and pinned as a PNG under `tests/golden/`, compared in
//! bytes. The chats list (slice one), and the open chat, a group's chat
//! and the compose line typed into with the M9's keys (slice two). A frame that moved fails and leaves the new picture beside the
//! test's target dir as `m9-golden/<name>`; `LOQUAX_WRITE_GOLDEN=1` writes
//! the goldens instead, for a change that means to move them.
//!
//! The board dumps the same frame over its debug link and the rig compares
//! it with the same file (item 3, a later slice).

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

use std::path::{Path, PathBuf};

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
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::keyboard::{self, Cap, Keyboard, Layout};
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

fn png(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer
        .write_image_data(&frame.to_rgb888())
        .expect("png data");
    writer.finish().expect("png end");
    out
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check(name: &str, frame: &Frame) {
    let got = png(frame);
    let path = golden(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read(&path).unwrap_or_default();
    if got != want {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m9-golden");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), &got);
        panic!(
            "{name} moved: {} B drawn against {} B pinned; the drawn one is {}",
            got.len(),
            want.len(),
            dir.join(name).display()
        );
    }
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
    // The top bar is one cell row of 10 px, each entry ENTRY_ROWS more.
    let band = (10 + row * ENTRY_ROWS * 10)..(10 + (row + 1) * ENTRY_ROWS * 10);
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
    for _ in 0..12 {
        press(&mut model, M9Key::Up);
    }
    let moved = text_of(&model);
    assert_ne!(moved[1], rows[1], "twelve bubbles up, the window moved");
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
        sent.trim_end().ends_with(" ."),
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

/// The layout `tests/keyboard/walk.txt` was written over, not the board's:
/// q to p on row 0, a to l on row 1, Shift, z to m and Sym on row 2,
/// Enter on row 3.
fn walk_layout() -> Layout {
    let mut cells = [[Cap::None; keyboard::COLUMNS]; keyboard::ROWS];
    for (column, (plain, sym)) in "qwertyuiop".chars().zip("1234567890".chars()).enumerate() {
        cells[0][column] = Cap::letter(plain, sym);
    }
    for (column, (plain, sym)) in "asdfghjkl".chars().zip("@#&*-+=()".chars()).enumerate() {
        cells[1][column] = Cap::letter(plain, sym);
    }
    cells[2][0] = Cap::Shift;
    for (column, (plain, sym)) in "zxcvbnm".chars().zip("!?,.;:'".chars()).enumerate() {
        cells[2][column + 1] = Cap::letter(plain, sym);
    }
    cells[2][9] = Cap::Sym;
    cells[3][4] = Cap::Key(M9Key::Enter);
    Layout(cells)
}

/// Slice three (1359): a recording of scan frames, through the keyboard
/// driver, the key table and the model, opens the chat, types "Hi!" with a
/// tapped Shift, a bounce and a tapped Sym, and sends it. The recording is
/// synthetic until the board records its own in the same format.
#[test]
fn a_recorded_walk_of_scan_frames_types_and_sends() {
    let home = tempfile::tempdir().expect("home");
    let mut model = on_the_list(home.path());
    let layout = walk_layout();
    let mut keys = Keyboard::new(&layout);
    let recording = include_str!("keyboard/walk.txt");
    let mut effects = Vec::new();
    for poll in keyboard::parse_recording(recording) {
        let (ms, scan) = poll.expect("a recorded poll");
        let mut events = Vec::new();
        keys.poll(ms, scan, &mut |e| events.push(e));
        for e in events {
            effects.extend(update(&mut model, UiEvent::Key(e)));
        }
    }
    let peer = model.conversations[model.selected].peer;
    assert!(
        effects.iter().any(|e| matches!(
            e,
            UiEffect::Send { to, body, .. } if *to == peer && body == "Hi!"
        )),
        "the walk sends Hi!: {effects:?}"
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
    for words in [
        "Anna",
        "Bob",
        "Brot ist alle",
        "Ich bring welches mit ?",
        "Bis gleich",
        "Wer kommt noch?",
        "arrived late",
        "Ich komme um acht",
    ] {
        assert!(has(words), "{words:?} is not on the screen: {rows:#?}");
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
