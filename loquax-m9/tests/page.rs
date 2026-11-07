//! The board's `[PAGE]` line on the host (1533): a walk over the fixture
//! with the board's own keys, drawn by one renderer as the board draws,
//! says where it is after every key, and says nothing when a key moved
//! nothing.

use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::fixture;
use loquax_m9::frame::Frame;
use loquax_m9::group;
use loquax_m9::keyboard::{self, Key};
use loquax_m9::map::{Map, FIXTURE};
use loquax_m9::page::{self, Page, Said};
use loquax_m9::render::Renderer;
use loquax_m9::screen::Look;

/// The board's key path for one controller code: the role's command, the
/// key's event through the group table (`thinknode_m9.rs`, `ui_main`).
fn press(model: &mut UiModel<Field>, row: &mut usize, code: u8) {
    let key = Key::of(code).expect("a key");
    if let Some(command) = key.command() {
        keyboard::run(model, row, command);
    }
    if let Some(event) = key.event() {
        group::press(model, row, &event);
    }
}

struct Board {
    model: UiModel<Field>,
    row: usize,
    said: Said,
}

impl Board {
    /// Draw as the board draws, and the `[PAGE]` body it would say.
    fn draw(&mut self, renderer: &mut Renderer<'_, Frame>, map: &mut Map<'_>) -> Option<String> {
        renderer
            .show(&self.model, &Look::of(&self.model), map)
            .expect("draw");
        let at = page::of(&self.model, self.row, false, &renderer.window());
        self.said.say(at)
    }

    fn key(&mut self, code: u8) {
        press(&mut self.model, &mut self.row, code);
    }
}

#[test]
fn a_walk_says_its_page_cursor_and_top_after_every_key() {
    let mut frame = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    let mut board = Board {
        model: fixture::model(),
        row: 0,
        said: Said::default(),
    };
    let first = board
        .draw(&mut renderer, &mut map)
        .expect("the boot's line");
    assert_eq!(first, "name=chats cursor=0 top=0");
    assert_eq!(
        board.draw(&mut renderer, &mut map),
        None,
        "nothing moved, nothing said"
    );

    // Ring down (0xb6) moves the list's cursor; ring up back.
    board.key(0xb6);
    assert_eq!(
        board.draw(&mut renderer, &mut map).as_deref(),
        Some("name=chats cursor=1 top=0")
    );
    // Down to the list's end: the top follows once the cursor leaves the
    // five entries the screen holds.
    let total = board.model.conversations.len();
    for _ in 0..total {
        board.key(0xb6);
    }
    let bottom = board.draw(&mut renderer, &mut map).expect("moved");
    let at = page::of(&board.model, 0, false, &renderer.window());
    assert_eq!(at.name, "chats");
    assert!(at.cursor >= 5, "{bottom}");
    assert!(at.top > 0, "the list scrolled: {bottom}");
    assert_eq!(at.top, at.cursor + 1 - 5, "{bottom}");
    for _ in 0..total {
        board.key(0xb5);
    }
    assert_eq!(
        board.draw(&mut renderer, &mut map).as_deref(),
        Some("name=chats cursor=0 top=0")
    );

    // The centre opens the pinned chat; the newest message under the
    // cursor, the screen ending at it.
    board.key(0x0d);
    let open = board.draw(&mut renderer, &mut map).expect("the chat");
    let chat = page::of(&board.model, 0, false, &renderer.window());
    assert_eq!(chat.name, "chat", "{open}");
    // Up through the chat: the cursor walks back, and the top follows it
    // once it passes the screen's first bubble.
    // The pinned chat is longer than a screen: the newest at the bottom,
    // the topmost shown some messages above it.
    assert!(chat.top > 0 && chat.top < chat.cursor, "{open}");
    let mut tops = vec![chat.top];
    for _ in 0..chat.cursor {
        board.key(0xb5);
        board.draw(&mut renderer, &mut map);
        tops.push(page::of(&board.model, 0, false, &renderer.window()).top);
    }
    let at = page::of(&board.model, 0, false, &renderer.window());
    assert_eq!((at.cursor, at.top), (0, 0), "at the chat's first message");
    assert!(tops.windows(2).all(|w| w[1] <= w[0]), "{tops:?}");
    assert!(
        tops.windows(2).any(|w| w[1] < w[0]),
        "it scrolled: {tops:?}"
    );

    // Back to the list, then the map key: the map and its zoom.
    board.key(0x86);
    assert!(board
        .draw(&mut renderer, &mut map)
        .expect("the list")
        .starts_with("name=chats"));
    board.key(0x85);
    let opened = board.draw(&mut renderer, &mut map).expect("the map");
    let zoom = |page: Page| page.zoom.expect("the map says its zoom");
    let z0 = zoom(page::of(&board.model, 0, false, &renderer.window()));
    assert_eq!(opened, format!("name=map cursor=0 top=0 zoom={z0}"));
    board.key(b'-');
    board.draw(&mut renderer, &mut map).expect("zoomed out");
    let z1 = zoom(page::of(&board.model, 0, false, &renderer.window()));
    assert_eq!(z1 + 1, z0, "'-' zooms out one step");
    board.key(0x81);
    assert!(board
        .draw(&mut renderer, &mut map)
        .expect("the list again")
        .starts_with("name=chats"));
}

/// The pairing screen over everything is `pair`; the compose line says
/// its caret.
#[test]
fn the_pairing_and_the_compose_line_have_their_names() {
    let mut model = fixture::model();
    let window = Default::default();
    assert_eq!(page::of(&model, 0, true, &window).name, "pair");
    let mut row = 0;
    press(&mut model, &mut row, 0x83);
    press(&mut model, &mut row, b'h');
    press(&mut model, &mut row, b'i');
    let at = page::of(&model, row, false, &window);
    assert_eq!((at.name, at.cursor), ("compose", 2));
}
