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
