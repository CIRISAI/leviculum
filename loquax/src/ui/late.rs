//! 1505: a late message sits at its writer's time, not where it arrived.
//! A message written at 09:00 that the mailbox hands on at 14:00 goes
//! above the 13:55 bubble, on every target, since every target draws the
//! model's body in its order; what names the body's messages by index
//! moves with them, the unread are the newest by arrival, and a reader
//! scrolled up keeps the rows on screen.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::tests::Field;
use super::{chat, update, view, Pane, UiEvent, UiModel};
use crate::outbox::OutboxEvent;
use crate::when::WallTime;

mod fixture {
    use crate as loquax;
    include!("../../tests/support/late_message.rs");
}

const ANNA: [u8; 16] = [0x6a; 16];
const DAY: i64 = 86_400;
const H: i64 = 3_600;
const M: i64 = 60;
/// A midnight, UTC.
const D0: i64 = 20_833 * DAY;

fn clock(unix_secs: i64) -> UiEvent {
    UiEvent::Clock(WallTime {
        unix_secs,
        offset_secs: 0,
    })
}

fn received(from: [u8; 16], n: u8, sent_at: i64, body: &str) -> UiEvent {
    UiEvent::Engine(OutboxEvent::Received {
        sent_at,
        message_id: [n; 32],
        source: from,
        title: Vec::new(),
        body: body.as_bytes().to_vec(),
        fields: Vec::new(),
        verified: true,
    })
}

fn bodies(model: &UiModel<Field>, c: usize) -> Vec<&str> {
    model.conversations[c]
        .messages
        .iter()
        .map(|m| m.body.as_str())
        .collect()
}

/// The terminal's conversation pane, as text, the rows that carry any.
fn screen(model: &UiModel<Field>) -> Vec<String> {
    view(model)
        .messages
        .iter()
        .map(|line| line.cells.iter().map(|c| c.ch).collect::<String>())
        .map(|row| row.trim().to_string())
        .filter(|row| !row.is_empty())
        .collect()
}

fn row_of(screen: &[String], words: &str) -> usize {
    screen
        .iter()
        .position(|row| row.contains(words))
        .unwrap_or_else(|| panic!("{words:?} is not on the screen: {screen:#?}"))
}

/// The order's cell: two messages, the earlier one delivered second
/// through the mailbox, as the client engine hands a pull on (signed LXMF,
/// `Command::Ingest`). The chat is open and follows. The 09:00 message
/// shows first, in the model, on the terminal's screen and after a
/// restart from the store; the read mark the arrival sends names it.
/// Red before 1505: the body was the arrival order.
#[test]
fn a_message_the_mailbox_hands_on_late_sits_at_its_writers_time() {
    use crate::engine::{delivery_address, Engine};
    use crate::outbox::Command;
    use leviculum_core::node::NodeCoreBuilder;
    use leviculum_core::DestinationHash;
    use leviculum_lxmf::DeliveryMethod;
    use leviculum_std::driver::{StdClock, StdStorage};

    let dir = tempfile::tempdir().expect("tempdir");
    let us = leviculum_std::generate_identity();
    let address = delivery_address(&us).expect("our address");
    let mut engine = Engine::client(address);
    let mut core = NodeCoreBuilder::new().enable_transport(false).build(
        rand_core::OsRng,
        StdClock::new(),
        StdStorage::new(dir.path().join("core")).expect("storage"),
    );
    let alice = leviculum_std::generate_identity();
    let alice_at = delivery_address(&alice).expect("alice");
    core.remember_identity(DestinationHash::new(alice_at), alice.clone());
    let message = |at: i64, body: &str| {
        leviculum_lxmf::Message::create(
            address,
            alice_at,
            &alice,
            at as f64,
            Vec::new(),
            body.as_bytes().to_vec(),
            Vec::new(),
            DeliveryMethod::Direct,
        )
        .expect("message")
    };
    let nine = message(D0 + 9 * H, "written at nine");
    let five_to_two = message(D0 + 13 * H + 55 * M, "written at five to two");
    // The mailbox's order: the later one first, the earlier one late.
    for m in [&five_to_two, &nine] {
        engine.submit(Command::Ingest { packed: m.pack() });
    }
    let now_ms = core.now_ms();
    let _ = engine.on_tick(&mut core, now_ms);
    let events: Vec<OutboxEvent> = core::iter::from_fn(|| engine.next_event()).collect();

    let mut model = UiModel::<Field>::new(&[]);
    model.home_client = true;
    model.focus = Pane::Messages;
    let mut taken = 0;
    for event in events {
        let arrival = matches!(event, OutboxEvent::Received { .. });
        if arrival {
            // 14:00 the first, 14:05 the late one.
            update(&mut model, clock(D0 + 14 * H + taken * 5 * M));
        }
        update(&mut model, UiEvent::Engine(event));
        if arrival {
            taken += 1;
            if taken == 1 {
                update(&mut model, UiEvent::Open { peer: alice_at });
            }
        }
    }
    assert_eq!(taken, 2, "both came");
    let c = model
        .conversations
        .iter()
        .position(|c| c.peer == alice_at)
        .expect("alice's chat");
    let check = |model: &UiModel<Field>, when: &str| {
        assert_eq!(
            bodies(model, c),
            ["written at nine", "written at five to two"],
            "{when}: the body is in its writers' order"
        );
        let screen = screen(model);
        assert!(
            row_of(&screen, "written at nine") < row_of(&screen, "written at five to two"),
            "{when}: the screen shows the earlier first: {screen:#?}"
        );
    };
    check(&model, "pulled");
    // Read where it arrived, the late one by its own id.
    let marked: Vec<[u8; 32]> = model.marks.iter().flat_map(|m| m.ids.clone()).collect();
    assert!(
        marked.contains(&nine.message_id),
        "the late one is marked read: {marked:?}"
    );

    #[cfg(feature = "history")]
    {
        let path = dir.path().join("history.sqlite3");
        let mut store = crate::store::Store::open(&path, address).expect("store");
        store.write(&model.take_writes()).expect("written");
        drop(store);
        let store = crate::store::Store::open(&path, address).expect("store again");
        let mut restarted = UiModel::<Field>::new(&[]);
        restarted.home_client = true;
        restarted.focus = Pane::Messages;
        update(&mut restarted, clock(D0 + 15 * H));
        restarted.restore(store.restore().expect("restored"));
        update(&mut restarted, UiEvent::Open { peer: alice_at });
        check(&restarted, "restarted");
    }
}

/// The fixture every target's golden draws: last night's day line over
/// last night's two, the late one second with the `new` rule above it,
/// then today's line and today's two. The rule stays where the reader
/// left off, by arrival: the late one is new to them, though the 13:55
/// below it was read. Opening the chat marks the late one read.
#[test]
fn the_late_one_sits_under_its_own_day_with_the_rule_above_it() {
    let mut model = UiModel::<Field>::new(&[]);
    model.address = Some([0x30; 16]);
    model.home_client = true;
    let c = fixture::late_message(&mut model, D0, 3_600);
    assert_eq!(
        bodies(&model, c),
        [
            "Gute Nacht",
            "Kommst du morgen mit?",
            "Bis gleich am Bahnhof",
            "Bin unterwegs"
        ]
    );
    let rule = model.conversations[c].new_rule.expect("the rule");
    assert_eq!((rule.at, rule.count), (1, 1), "above the late one");
    let days: Vec<bool> = chat::placements(&chat::bubbles(&model, c))
        .iter()
        .map(|p| p.day)
        .collect();
    assert_eq!(
        days,
        [true, false, true, false],
        "the days are the writers'"
    );
    let marked: Vec<[u8; 32]> = model.marks.iter().flat_map(|m| m.ids.clone()).collect();
    assert!(
        marked.contains(&fixture::LATE_LATE),
        "opening read the late one: {marked:?}"
    );
}

/// The unread are the newest by arrival (1505): a late one is the
/// conversation's one unread wherever the writers' order puts it, its
/// badge counts it, and another device's read mark through it reads it,
/// though messages written after it (read long ago) sit below it.
#[test]
fn the_unread_are_the_newest_by_arrival_wherever_they_sit() {
    let mut model = UiModel::<Field>::new(&[]);
    model.home_client = true;
    update(&mut model, clock(D0 + 13 * H));
    update(&mut model, received(ANNA, 1, D0 + 8 * H, "eight"));
    update(&mut model, received(ANNA, 2, D0 + 12 * H, "twelve"));
    update(&mut model, UiEvent::Open { peer: ANNA });
    // The list a screen of its own (the phone, the M9): an arrival opens
    // nothing.
    model.list_apart = true;
    update(&mut model, UiEvent::Close);
    update(&mut model, clock(D0 + 14 * H));
    update(&mut model, received(ANNA, 3, D0 + 9 * H, "nine, late"));
    let c = model
        .conversations
        .iter()
        .position(|c| c.peer == ANNA)
        .expect("anna");
    let conversation = &model.conversations[c];
    assert_eq!(bodies(&model, c), ["eight", "nine, late", "twelve"]);
    assert_eq!(conversation.unread, 1);
    assert_eq!(conversation.unread_rows(), 1);
    assert_eq!(conversation.new_rule.map(|r| r.at), Some(1));
    assert_eq!(
        conversation.unread_messages(),
        [1],
        "the late one is unread"
    );
    assert_eq!(
        conversation.rule_from_unread().map(|r| r.at),
        Some(1),
        "a restart puts the rule where the screen had it"
    );
    // Another device read through the late one: nothing is unread.
    model.read_through(&[3; 32]);
    assert_eq!(model.conversations[c].unread, 0);
    assert!(model.conversations[c].new_rule.is_none());
}

/// What names the body's messages by index moves with them: a send in
/// flight below a late arrival is still the one its `Queued` and its end
/// name, and the notes stand before the messages they stood before.
#[test]
fn a_late_arrival_moves_the_sends_and_the_notes_with_their_messages() {
    let mut model = UiModel::<Field>::new(&[]);
    update(&mut model, clock(D0 + 13 * H));
    update(&mut model, received(ANNA, 1, D0 + 12 * H, "twelve"));
    update(&mut model, UiEvent::Open { peer: ANNA });
    let c = model.selected;
    model.conversations[c].notes.push(super::Note {
        before: 0,
        text: "before twelve".into(),
    });
    update(
        &mut model,
        UiEvent::Write {
            peer: ANNA,
            body: "ours".into(),
        },
    );
    assert_eq!(model.sending.front(), Some(&(c, 1)));
    update(&mut model, received(ANNA, 2, D0 + 9 * H, "nine, late"));
    assert_eq!(bodies(&model, c), ["nine, late", "twelve", "ours"]);
    assert_eq!(model.sending.front(), Some(&(c, 2)), "the send moved");
    assert_eq!(model.conversations[c].notes[0].before, 1, "the note moved");
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Queued {
            message_id: [0x77; 32],
        }),
    );
    let messages = &model.conversations[c].messages;
    assert_eq!(messages[2].id, Some([0x77; 32]), "ours took its id");
    assert_eq!(messages[0].id, Some([2; 32]), "the late one kept its own");
}

/// A reader scrolled up in the terminal keeps the rows on screen when a
/// late bubble lands above them (1505); one landing under the first row
/// on screen moves nothing above it, and a reader at the newest stays at
/// the newest.
#[test]
fn a_late_bubble_above_the_view_moves_no_row_on_screen() {
    let mut model = UiModel::<Field>::new(&[]);
    model.screen = (100, 30);
    update(&mut model, clock(D0 + 20 * H));
    for n in 0..40u8 {
        let at = D0 + 6 * H + i64::from(n) * 10 * M;
        update(
            &mut model,
            received(ANNA, n + 1, at, &alloc::format!("line {n}")),
        );
    }
    update(&mut model, UiEvent::Open { peer: ANNA });
    model.focus = Pane::Messages;
    let c = model.selected;
    // Scrolled up to "line 20" at the top.
    let laid = model.laid();
    let top = laid.spans[20].start;
    model.scroll_top = Some(top);
    model.message_cursor = 25;
    let before = screen(&model);
    assert!(
        before[0].contains("line 20") || before[1].contains("line 20"),
        "{before:#?}"
    );
    // Written between line 2 and line 3: above the view.
    update(
        &mut model,
        received(ANNA, 100, D0 + 6 * H + 25 * M, "late above"),
    );
    assert_eq!(model.conversations[c].messages[3].body, "late above");
    assert_eq!(screen(&model), before, "no row on screen moved");
    assert_eq!(model.message_cursor, 26, "the cursor stays on its message");
    // Written below the view's top: the rows above it stay, the top too.
    let top = model.scroll_top;
    update(
        &mut model,
        received(ANNA, 101, D0 + 6 * H + 395 * M, "late below"),
    );
    assert_eq!(model.scroll_top, top);
    // At the newest: still at the newest.
    model.scroll_top = None;
    update(
        &mut model,
        received(ANNA, 102, D0 + 6 * H + 5 * M, "late, following"),
    );
    assert_eq!(model.scroll_top, None);
    assert_eq!(
        model.message_cursor,
        model.conversations[c].messages.len() - 1
    );
}

/// A page the store hands out in arrival order is put in the writers'
/// order, and the store's next page asks below the lowest key held, not
/// below the first bubble's.
#[test]
fn a_restored_page_is_in_its_writers_order_and_pages_on_from_its_lowest_key() {
    let mut model = UiModel::<Field>::new(&[]);
    update(&mut model, clock(D0 + 20 * H));
    update(&mut model, received(ANNA, 1, D0 + 12 * H, "twelve"));
    update(&mut model, received(ANNA, 2, D0 + 9 * H, "nine, late"));
    let c = 0;
    let mut stored = model.conversations[c].messages.to_vec();
    stored.sort_by_key(|m| m.activity);
    assert_eq!(stored[0].body, "twelve", "the store's order is arrival");
    let lowest = stored[0].activity;
    let mut restarted = UiModel::<Field>::new(&[]);
    update(&mut restarted, clock(D0 + 20 * H));
    restarted.restore(alloc::vec![super::history::Restored {
        peer: ANNA,
        pinned: false,
        flags: super::history::Flags::default(),
        unread: 0,
        messages: stored,
        earlier: 5,
        draft: super::history::Draft::default(),
        group: None,
        listing: None,
    }]);
    assert_eq!(bodies(&restarted, 0), ["nine, late", "twelve"]);
    assert_eq!(restarted.conversations[0].oldest_key(), Some(lowest));
    assert_eq!(
        restarted.loaded_from().get(&ANNA),
        Some(&lowest),
        "the store's search looks below the lowest key"
    );
}

/// A group's body is its home's acceptance order, the order every member
/// sees (`crate::home::group`): a copy whose writer wrote it before the
/// copy above it still goes last, and shows its writer's time. Only a
/// chat with one person runs in its writers' order.
#[test]
fn a_groups_copy_keeps_the_homes_acceptance_order() {
    use leviculum_lxmf::lev::{GroupCopy, Lev};
    const GROUP: [u8; 16] = [0x7a; 16];
    let copy = |n: u8, writer: [u8; 16], written: i64, accepted: i64, body: &str| {
        let mut fields = Vec::new();
        Lev {
            group_copy: Some(GroupCopy {
                sender: writer,
                sent_ms: written as u64 * 1000,
                epoch: 1,
            }),
            ..Lev::default()
        }
        .put_into(&mut fields);
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: accepted,
            message_id: [n; 32],
            source: GROUP,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields,
            verified: true,
        })
    };
    let mut model = UiModel::<Field>::new(&[]);
    update(&mut model, clock(D0 + 14 * H));
    update(
        &mut model,
        copy(1, [1; 16], D0 + 9 * H, D0 + 9 * H, "accepted first"),
    );
    update(
        &mut model,
        copy(
            2,
            [2; 16],
            D0 + 8 * H,
            D0 + 13 * H,
            "written earlier, accepted second",
        ),
    );
    let c = model
        .conversations
        .iter()
        .position(|c| c.peer == GROUP)
        .expect("the group");
    assert_eq!(
        bodies(&model, c),
        ["accepted first", "written earlier, accepted second"]
    );
    assert_eq!(
        model.conversations[c].messages[1].at.map(|at| at.unix_secs),
        Some(D0 + 8 * H),
        "it shows its writer's time"
    );
}
