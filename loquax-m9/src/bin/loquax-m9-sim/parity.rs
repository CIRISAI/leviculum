//! The sim draws what the board draws (1530): every screen the goldens
//! pin (`tests/golden/m9-fw-*.png`, the files the rig compares the
//! board's `FRAME_DUMP` with), reached through the sim's board, compared
//! pixel for pixel. Two ways in: the board's own `FRAME_DUMP` path for
//! all eleven fixture screens and the pairing ([`Board::show_screen`]),
//! and for the four the board's first model reaches, its keys pressed
//! from power-on, as a finger presses them (the controller's code, the
//! poll queue, the key table).

use std::path::PathBuf;

use loquax_m9::fixture::{Screen, SCREENS};
use loquax_m9::frame::Frame;
use loquax_m9::keys::M9Key;
use loquax_m9::pair;

use crate::board::{self, Board, Log};
use crate::flash::Flash;

/// The golden `name`, as RGB888 bytes.
fn golden(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let file = std::fs::File::open(&path).expect("the golden");
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("a png");
    let mut buf = vec![0; reader.output_buffer_size().expect("its size")];
    let info = reader.next_frame(&mut buf).expect("its pixels");
    assert_eq!(info.color_type, png::ColorType::Rgb, "{name}");
    buf.truncate(info.buffer_size());
    buf
}

/// Whether `frame` is the golden `name`; the differing pixels when not.
fn same(frame: &Frame, name: &str) -> Result<(), usize> {
    let want = golden(name);
    let got = frame.to_rgb888();
    assert_eq!(got.len(), want.len());
    let off = got
        .chunks(3)
        .zip(want.chunks(3))
        .filter(|(a, b)| a != b)
        .count();
    match off {
        0 => Ok(()),
        n => Err(n),
    }
}

fn fresh() -> Board {
    Board::boot(Flash::blank(), Log::default(), 1).expect("the board boots")
}

#[test]
fn every_frame_dump_is_its_golden() {
    let mut board = fresh();
    let mut screens: Vec<(u8, String)> = SCREENS
        .iter()
        .map(|screen| (screen.byte(), format!("m9-fw-{}.png", screen.name())))
        .collect();
    screens.push((pair::DUMP_BYTE, format!("m9-fw-{}.png", pair::DUMP_NAME)));
    let mut off = Vec::new();
    for (byte, name) in &screens {
        board.show_screen(*byte).expect("a screen byte");
        if let Err(n) = same(board.frame(), name) {
            off.push(format!("{name}: {n} pixels"));
        }
        // The board's next turn draws what it shows: the walked model.
        board.settle();
    }
    assert!(off.is_empty(), "the sim's frames differ: {off:?}");
    // The positive control: a frame that is not the golden fails.
    board.show_screen(Screen::Chat.byte());
    assert!(same(board.frame(), "m9-fw-chats.png").is_err());
}

/// The controller's code of a key in a fixture walk.
fn code(key: M9Key) -> u8 {
    match key {
        M9Key::Up => 0xb5,
        M9Key::Down => 0xb6,
        M9Key::Left => 0xb4,
        M9Key::Right => 0xb7,
        M9Key::Enter => 0x0d,
        M9Key::Back => 0x86,
        M9Key::Backspace => 0x08,
        M9Key::Char(c) => c as u8,
    }
}

#[test]
fn the_boards_keys_from_power_on_reach_the_goldens() {
    for screen in [Screen::Chats, Screen::Chat, Screen::Map, Screen::Pan] {
        let mut board = fresh();
        assert_eq!(
            same(board.frame(), "m9-fw-pairing.png"),
            Ok(()),
            "a board without a record opens on the pairing"
        );
        // Back leaves the pairing for the fixture.
        let mut now = 100;
        let mut press = |board: &mut Board, code: u8| {
            board::set_clock_ms(now);
            board.press(code);
            board.settle();
            now += 150;
        };
        press(&mut board, 0x86);
        for key in screen.keys() {
            press(&mut board, code(key));
        }
        // The map's key hint and credit stand by the clock (1531); the
        // goldens are the map without them.
        board::set_clock_ms(now + loquax_m9::map::CREDIT_MS + 2 * loquax_m9::map::HINT_MS);
        board.settle();
        let name = format!("m9-fw-{}.png", screen.name());
        assert_eq!(same(board.frame(), &name), Ok(()), "{name}");
    }
}

#[test]
fn a_read_survives_the_sims_reboot() {
    // `--store real`'s path in small: a home's board, one page pulled,
    // the chat opened, the power cycled on the same flash.
    let mut flash = Flash::blank();
    flash.home = Some(crate::real::address());
    let mut board = Board::boot(flash, Log::default(), 1).expect("boot");
    let archive = crate::real::archive().expect("the fixture's archive");
    for page in archive.chunks(72) {
        board.pull(page);
        board.settle();
    }
    let unread = |board: &Board| {
        board
            .text()
            .iter()
            .filter(|row| row.contains('[') && row.trim_end().ends_with(']'))
            .count()
    };
    let before = unread(&board);
    assert!(
        before > 0,
        "the fixture has unread chats on the first screen"
    );
    let (flash, log) = board.power_off();
    let board = Board::boot(flash, log, 2).expect("the reboot");
    assert!(
        board.log.kept.iter().any(|l| l.contains("restore=resumed")),
        "the second boot resumes from the summary log"
    );
    assert_eq!(unread(&board), before, "the list as it was");
}

/// A home's board after its first pull of the 1489 store, and the pull's
/// log lines: `--store real`'s path.
fn pulled() -> Board {
    let mut flash = Flash::blank();
    flash.home = Some(crate::real::address());
    let mut board = Board::boot(flash, Log::default(), 1).expect("boot");
    let archive = crate::real::archive().expect("the fixture's archive");
    for page in archive.chunks(72) {
        board.pull(page);
        board.settle();
    }
    board
}

/// That board power-cycled: the store resumed from its summary log, no
/// conversation's messages loaded.
fn resumed() -> Board {
    let (flash, log) = pulled().power_off();
    let mut board = Board::boot(flash, log, 2).expect("the reboot");
    // The loop's first turns, as `headless` runs them before a key.
    board.settle();
    assert!(
        board.log.kept.iter().any(|l| l.contains("restore=resumed")),
        "the second boot resumes from the summary log"
    );
    board
}

/// Press `code` at `ms`, and the log lines the press left.
fn press(board: &mut Board, ms: u64, code: u8) -> Vec<String> {
    let from = board.log.kept.len();
    board::set_clock_ms(ms);
    board.press(code);
    board.settle();
    board.log.kept[from..].to_vec()
}

/// The selected conversation's peer and unread.
fn selected(board: &Board) -> ([u8; 16], usize) {
    let model = board.model();
    let c = &model.conversations[model.selected];
    (c.peer, c.unread)
}

fn unread_of(board: &Board, peer: [u8; 16]) -> usize {
    board
        .model()
        .conversations
        .iter()
        .find(|c| c.peer == peer)
        .map_or(0, |c| c.unread)
}

fn marked(lines: &[String]) -> bool {
    lines.iter().any(|l| l.starts_with("[STORE] marks="))
}

#[test]
fn a_resumed_lists_cursor_reads_nothing_it_moves_onto() {
    // 1546 item 1: ring down onto Anna (3 unread) with the list still up.
    let mut board = resumed();
    let lines = press(&mut board, 150, 0xb6);
    let (anna, unread) = selected(&board);
    assert!(
        lines.iter().any(|l| l.starts_with("[PAGE] name=chats ")),
        "the keys stay on the list: {lines:?}"
    );
    assert_eq!(unread, 3, "the cursor's chat keeps its unread: {lines:?}");
    assert!(
        !marked(&lines),
        "no read mark for a chat not opened: {lines:?}"
    );
    // The positive control: Enter opens it, and that reads it.
    let lines = press(&mut board, 300, 0x0d);
    assert!(marked(&lines), "opening reads: {lines:?}");
    assert_eq!(unread_of(&board, anna), 0);
}

#[test]
fn a_group_opened_after_a_resume_is_marked_read_and_stays_read() {
    // 1546 item 2: the list's first row after the boot, the group with 7.
    let mut board = resumed();
    let (group, unread) = selected(&board);
    assert_eq!(unread, 7, "the boot's cursor stands on the group");
    // Its newest page was read at boot, for the cursor's row, and marked
    // nothing.
    assert!(
        board
            .log
            .kept
            .iter()
            .any(|l| l.starts_with("[STORE] page_read entries=")),
        "the boot loads the cursor's chat"
    );
    assert!(!marked(&board.log.kept), "the boot marks nothing read");
    assert_eq!(unread_of(&board, group), 7);
    let lines = press(&mut board, 150, 0x0d);
    assert!(
        lines.iter().any(|l| l.starts_with("[PAGE] name=chat ")),
        "Enter opened the chat: {lines:?}"
    );
    assert!(marked(&lines), "opening the group marks it read: {lines:?}");
    assert_eq!(unread_of(&board, group), 0);
    press(&mut board, 300, 0x86);
    let (flash, log) = board.power_off();
    let board = Board::boot(flash, log, 3).expect("the second reboot");
    assert_eq!(
        unread_of(&board, group),
        0,
        "the read survives the power cycle"
    );
}

/// The list's cursor line on the panel: `cursor - top` of a `[PAGE]`
/// line of the chats.
fn line_of(page: &str) -> Option<usize> {
    let field = |name: &str| -> Option<usize> {
        page.split_whitespace()
            .find_map(|w| w.strip_prefix(name))
            .and_then(|v| v.parse().ok())
    };
    match page.starts_with("name=chats ") {
        true => Some(field("cursor=")? - field("top=")?),
        false => None,
    }
}

#[test]
fn a_pull_never_moves_the_lists_cursor() {
    // 1546 item 3: 61 pages fold while the list is up and no key is
    // pressed. The row under the cursor keeps it, on its line.
    let mut flash = Flash::blank();
    flash.home = Some(crate::real::address());
    let mut board = Board::boot(flash, Log::default(), 1).expect("boot");
    let archive = crate::real::archive().expect("the fixture's archive");
    let mut held: Option<([u8; 16], usize)> = None;
    let mut moved = Vec::new();
    for (i, page) in archive.chunks(72).enumerate() {
        board.pull(page);
        board.settle();
        if board.model().conversations.is_empty() {
            continue;
        }
        let peer = selected(&board).0;
        let line = board
            .last_page
            .as_deref()
            .and_then(line_of)
            .expect("the list's page line");
        match held {
            None => held = Some((peer, line)),
            Some(was) if was != (peer, line) => moved.push(format!(
                "page {i}: {:?} -> {:?}",
                (&was.0[..3], was.1),
                (&peer[..3], line)
            )),
            Some(_) => {}
        }
    }
    assert!(moved.is_empty(), "the cursor moved by itself: {moved:?}");
}
