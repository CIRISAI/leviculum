//! 1541: the M9 reads by its viewport. The board draws, says the model
//! which bubbles' last lines were on screen ([`Reader::say`]), and tells
//! it the clocks ([`Reader::tick`]); a bubble whose last line stood on
//! screen for the dwell is seen, and the board, a device of its home,
//! hands the home a seen mark for it. Opening the chat alone reads nothing
//! further than its first page shows.

use leviculum_core::Identity;
use leviculum_lxmf::lev::Lev;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::board::store::MemFlash;
use loquax::home::wire::{Entry, Mark, MarkFlag};
use loquax::home::{lxmf_address, KIND_RECEIVED};
use loquax::ui::receipts::{BATCH_MS, SEEN, SEEN_MS};
use loquax::ui::{update, Pane, UiEffect, UiEvent, UiModel};
use loquax_m9::field::Field;
use loquax_m9::frame::Frame;
use loquax_m9::group;
use loquax_m9::home;
use loquax_m9::keys::{event, M9Key};
use loquax_m9::read::Reader;
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look};
use rand_core::OsRng;

/// The home's time on the entries: unix seconds.
const HOME_AT: i64 = 1_800_000_000;

/// The board's side of a walk: its model, its reader, its uptime, and
/// the seen marks it routed to the home.
struct Board {
    model: UiModel<Field>,
    reader: Reader,
    row: usize,
    ms: u64,
    marks: Vec<Mark>,
}

impl Board {
    fn new(model: UiModel<Field>) -> Self {
        let mut reader = Reader::default();
        reader.saw(
            &[Entry {
                seq: 1,
                kind: KIND_RECEIVED,
                at: HOME_AT,
                packed: Vec::new(),
            }],
            0,
        );
        Self {
            model,
            reader,
            row: 0,
            ms: 0,
            marks: Vec::new(),
        }
    }

    /// What a fold left, routed as the board's loop routes it.
    fn route(&mut self, effects: Vec<UiEffect>) {
        let routed = home::route(&mut self.model, effects, &mut || [7; 16]);
        self.marks.extend(routed.marks);
    }

    /// One turn at uptime `ms`: the clocks, then the draw and what it
    /// showed, as the loop does.
    fn turn(&mut self, renderer: &mut Renderer<'_, Frame>, ms: u64) {
        self.ms = ms;
        let (ticked, _) = self.reader.tick(&mut self.model, ms);
        self.route(ticked);
        renderer
            .draw(&screen::scene(&self.model), &Look::of(&self.model))
            .expect("draw");
        let said = self.reader.say(&mut self.model, renderer.seen(), ms);
        self.route(said);
    }

    /// A key at the current uptime, as the board presses it, then its
    /// frame.
    fn key(&mut self, renderer: &mut Renderer<'_, Frame>, key: M9Key) {
        let effects = group::press(&mut self.model, &mut self.row, &event(key));
        self.route(effects);
        let ms = self.ms;
        self.turn(renderer, ms);
    }

    /// The ids the seen marks routed so far name, and their times.
    fn seen_marks(&self) -> Vec<([u8; 32], u32)> {
        self.marks
            .iter()
            .filter_map(|mark| match mark.flag {
                MarkFlag::Seen { at } => Some(mark.ids.iter().map(move |id| (*id, at))),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Whether the message `id` of the open chat is seen on the board.
    fn is_seen(&self, id: [u8; 32]) -> bool {
        self.model.conversations[self.model.selected]
            .messages
            .iter()
            .find(|m| m.id == Some(id))
            .is_some_and(|m| m.ledger.iter().any(|e| e.what == SEEN))
    }
}

/// A message from `peer` with the read-receipt mark, `id` its LXMF id.
fn marked(peer: [u8; 16], id: [u8; 32], body: &str) -> UiEvent {
    UiEvent::Engine(loquax::outbox::OutboxEvent::Received {
        sent_at: 0,
        message_id: id,
        source: peer,
        title: Vec::new(),
        body: body.as_bytes().to_vec(),
        fields: Lev::mark().fields().to_vec(),
        verified: true,
    })
}

const BEFORE: [u8; 32] = [1; 32];
const LONG: [u8; 32] = [2; 32];
const AFTER: [u8; 32] = [3; 32];

/// The chat of 1529's golden (`tests/golden.rs`,
/// `a_long_message_is_read_by_the_line_on_the_m9`): a short message, one
/// over three screens long, a short one; every one marked, on a board
/// that is a device of its home, the keys on the list.
fn long_chat() -> UiModel<Field> {
    let peer = [0x4f; 16];
    let mut model = UiModel::<Field>::new(&[peer]);
    model.home_client = true;
    model.list_apart = true;
    update(
        &mut model,
        UiEvent::Viewing(screen::viewing(screen::Size::DEFAULT)),
    );
    let long: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    for (id, body) in [
        (BEFORE, "short before"),
        (LONG, long.join("\n").as_str()),
        (AFTER, "short after"),
    ] {
        update(&mut model, marked(peer, id, body));
    }
    model.toasts.clear();
    model.focus = Pane::Conversations;
    model
}

/// 1529's walk with the board's own draw: the chat opened and taken to
/// its top within the moment, so nothing under it stood for the dwell;
/// the top read for the dwell; then down a line a press. Twenty presses in, the long one's end is not on yet and
/// it is not read, however long the reader stays. Once its last line came
/// on, it is read at the dwell, not a moment before; the short one above,
/// on screen the whole way, is read; the short one under it, on screen
/// only for the moment of the open and never scrolled to again, is not.
/// The seen mark goes to the home when the chat is left, with the home's
/// time carried forward by the board's uptime.
#[test]
fn a_long_message_is_read_once_its_last_line_showed_for_the_dwell() {
    let mut frame = Frame::new();
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    let mut board = Board::new(long_chat());
    board.turn(&mut renderer, 0);
    board.key(&mut renderer, M9Key::Enter);
    assert_eq!(board.model.focus, Pane::Messages);
    // The open is the newest at the bottom: the short one under the long
    // one is on screen, its last line counting.
    let open = board.model.reading.viewport().cloned().expect("said");
    let after_key = board.model.conversations[board.model.selected]
        .messages
        .iter()
        .find(|m| m.id == Some(AFTER))
        .map(|m| m.activity)
        .expect("after");
    assert!(open.1.contains(&after_key), "{open:?}");
    for _ in 0..80 {
        board.key(&mut renderer, M9Key::Up);
    }
    assert_eq!(board.model.message_cursor, 0);
    assert!(
        !board.is_seen(AFTER),
        "on screen only for the moment of the open"
    );
    // The reader reads the top for the dwell: the short one there is read.
    let mut ms = SEEN_MS;
    board.turn(&mut renderer, ms);
    assert!(board.is_seen(BEFORE), "its last line stood for the dwell");
    // Down a line a press, a tenth of a second apart.
    for _ in 0..20 {
        ms += 100;
        board.ms = ms;
        board.key(&mut renderer, M9Key::Down);
    }
    assert_eq!(
        board.model.message_cursor, 1,
        "the long one, its end not on"
    );
    // The reader stays on this screen, long past the dwell.
    ms += 10 * SEEN_MS;
    board.turn(&mut renderer, ms);
    assert!(!board.is_seen(LONG), "its last line never showed");
    // Down until the long one's last line is on.
    let long_key = board.model.conversations[board.model.selected]
        .messages
        .iter()
        .find(|m| m.id == Some(LONG))
        .map(|m| m.activity)
        .expect("long");
    let mut presses = 0;
    while !board
        .model
        .reading
        .viewport()
        .is_some_and(|(_, keys)| keys.contains(&long_key))
    {
        presses += 1;
        assert!(presses < 60, "the long one's end never came on");
        board.key(&mut renderer, M9Key::Down);
    }
    assert!(presses > 1, "its end came by the line, not at once");
    let shown_at = board.ms;
    board.turn(&mut renderer, shown_at + SEEN_MS - 1);
    assert!(!board.is_seen(LONG), "not a moment before the dwell");
    board.turn(&mut renderer, shown_at + SEEN_MS);
    assert!(board.is_seen(LONG), "read at the dwell");
    assert!(!board.is_seen(AFTER), "never scrolled to for the dwell");
    // Back to the list: the seen mark goes to the home the batch's wait
    // after the last one seen.
    let left_at = board.ms;
    board.key(&mut renderer, M9Key::Back);
    board.turn(&mut renderer, left_at + BATCH_MS);
    let seen = board.seen_marks();
    let ids: Vec<[u8; 32]> = seen.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&LONG) && ids.contains(&BEFORE), "{ids:?}");
    assert!(!ids.contains(&AFTER), "{ids:?}");
    // The reader's time: the home's stamp carried forward by the uptime
    // of the first one seen.
    for (_, at) in &seen {
        let at = i64::from(*at);
        assert!(
            (HOME_AT..=HOME_AT + (left_at / 1000) as i64).contains(&at),
            "{at}"
        );
    }
}

/// The control of the walk above: a board that never says what it drew
/// (before 1541) reads nothing by the line. The same open, the same keys,
/// the same time: no bubble is seen and no seen mark goes.
#[test]
fn without_the_viewport_nothing_is_seen() {
    let mut frame = Frame::new();
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    let mut board = Board::new(long_chat());
    let mut row = 0;
    group::press(&mut board.model, &mut row, &event(M9Key::Enter));
    for _ in 0..80 {
        group::press(&mut board.model, &mut row, &event(M9Key::Down));
    }
    let (ticked, _) = board.reader.tick(&mut board.model, 10 * SEEN_MS);
    board.route(ticked);
    renderer
        .draw(&screen::scene(&board.model), &Look::of(&board.model))
        .expect("draw");
    assert!(renderer.seen().is_some_and(|keys| !keys.is_empty()));
    assert!(!board.is_seen(LONG) && !board.is_seen(BEFORE));
    assert!(board.seen_marks().is_empty());
}

/// A received message from `friend` to `address` with the mark, packed as
/// the home archives it.
fn pack(friend: &Identity, address: [u8; 16], body: &str) -> Vec<u8> {
    let source = lxmf_address(&friend.public_key_bytes()).expect("address");
    Message::create(
        address,
        source,
        friend,
        1.0,
        Vec::new(),
        body.as_bytes().to_vec(),
        Lev::mark().fields().to_vec(),
        DeliveryMethod::Direct,
    )
    .expect("create")
    .pack()
}

/// The board reads, by the viewport, the one message of a chat; leaves;
/// the seen mark is routed. Whether that mark is kept in the store
/// before the reboot is `keep`. After the reboot the board reads the chat
/// again: the seen marks it routes then.
fn read_reboot_read(keep: bool) -> Vec<([u8; 32], u32)> {
    let me = Identity::generate(&mut OsRng);
    let friend = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let page = vec![Entry {
        seq: 1,
        kind: KIND_RECEIVED,
        at: HOME_AT,
        packed: pack(&friend, address, "gelesen?"),
    }];
    let mut flash = MemFlash::new(16);
    let home::Boot {
        mut model,
        shelf: mut store,
        ..
    } = home::restore(&mut flash, address).expect("boot");
    home::take(&mut model, &page, &address);
    home::keep(&mut store, &mut flash, &page, &address).expect("keep");

    let walk = |model: UiModel<Field>| -> (UiModel<Field>, Vec<Mark>) {
        let mut frame = Frame::new();
        let mut renderer = Renderer::new(&mut frame).expect("renderer");
        let mut board = Board::new(model);
        board.reader.saw(&page, 0);
        board.turn(&mut renderer, 0);
        board.key(&mut renderer, M9Key::Enter);
        board.turn(&mut renderer, 2 * SEEN_MS);
        board.key(&mut renderer, M9Key::Back);
        board.turn(&mut renderer, 2 * SEEN_MS + BATCH_MS);
        (board.model, board.marks)
    };
    let (model, marks) = walk(model);
    let seen: Vec<Mark> = marks
        .into_iter()
        .filter(|m| matches!(m.flag, MarkFlag::Seen { .. }))
        .collect();
    assert_eq!(seen.len(), 1, "the viewport read it: {seen:?}");
    drop(model);
    if keep {
        home::keep_marks(&mut store, &mut flash, &seen).expect("kept");
    }
    let home::Boot { model, .. } = home::restore(&mut flash, address).expect("reboot");
    let (_, again) = walk(model);
    let mut board = Board::new(UiModel::<Field>::new(&[]));
    board.marks = again;
    board.seen_marks()
}

/// 1493's `KIND_MARK` keeps the viewport's seen mark (1541): after a
/// reboot the message read on the board owes no second receipt, home
/// heard from or not. The control: the same walk without the kept mark
/// sends the seen mark again after the reboot.
#[test]
fn a_seen_mark_from_the_viewport_survives_a_reboot() {
    assert_eq!(read_reboot_read(true), Vec::new(), "seen stays seen");
    assert_eq!(
        read_reboot_read(false).len(),
        1,
        "without keep_marks the reboot forgets the seen"
    );
}
