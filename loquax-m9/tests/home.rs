//! The M9 as a device of its home, on the screen's side (1415): the word
//! in the top bar, the pairing screen the code is typed on, the archive's
//! pages becoming the chats list and a chat, and a line typed on the board
//! becoming the home's `/send`.

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use leviculum_ui::style::RLine;
use loquax::home::board::Word;
use loquax::home::code::HomeCode;
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::strings::Lang;
use loquax::ui::{update, Pane, UiEvent};
use loquax_m9::home::{self, Outgoing};
use loquax_m9::keys::{event, M9Key};
use loquax_m9::pair::{self, Pairing};
use loquax_m9::screen::{self, Look, COLS, ROWS};
use rand_core::OsRng;

fn row_text(line: &RLine) -> String {
    line.cells.iter().map(|c| c.ch).collect()
}

fn pack(from: &Identity, to: [u8; 16], body: &str, at: f64) -> Vec<u8> {
    let source = lxmf_address(&from.public_key_bytes()).expect("address");
    Message::create(
        to,
        source,
        from,
        at,
        Vec::new(),
        body.as_bytes().to_vec(),
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("create")
    .pack()
}

/// The word stands at the bar's right end and the title gives way to it;
/// the bar is the grid's width whatever the word; without a home the bar
/// is the one the goldens pin.
#[test]
fn the_top_bar_says_the_home_at_its_right_end() {
    let me = Identity::generate(&mut OsRng);
    let model = home::model(lxmf_address(&me.public_key_bytes()).expect("address"));
    let scene = screen::scene(&model);
    let bar = |word: Option<Word>, lang| {
        let look = Look {
            home: word,
            lang,
            ..Look::of(&model)
        };
        row_text(&screen::lines(&scene, &look, COLS, ROWS)[0])
    };
    let none = bar(None, Lang::En);
    for (word, said) in [
        (Word::Joining, "Joining your mailbox"),
        (Word::Unreachable, "Mailbox unreachable"),
        (
            Word::Fetched {
                taken: 12,
                of: Some(40),
            },
            "Fetched 12 of 40",
        ),
    ] {
        let line = bar(Some(word), Lang::En);
        assert_eq!(line.chars().count(), COLS, "{line:?}");
        assert!(line.trim_end().ends_with(said), "{line:?}");
        assert_ne!(line, none);
    }
    let german = bar(
        Some(Word::Fetched {
            taken: 3,
            of: Some(9),
        }),
        Lang::De,
    );
    assert!(
        german.trim_end().ends_with("3 von 9 abgeholt"),
        "{german:?}"
    );
    // A sentence longer than the bar is cut, the bar still whole.
    let long = bar(Some(Word::CodeRefused), Lang::En);
    assert_eq!(long.chars().count(), COLS, "{long:?}");
}

/// The pairing screen: the code in fives, the verdict, the keys, the
/// grid's size exactly; once the code is taken the bar says the join.
#[test]
fn the_pairing_screen_shows_the_code_the_verdict_and_the_join() {
    let code = HomeCode {
        public_key: core::array::from_fn(|i| i as u8),
        token: [7; 16],
    };
    let text = code.encode();
    let mut pairing = Pairing::new();
    let mut got = None;
    for c in text.chars().take(23) {
        got = got.or(pairing.key(&event(M9Key::Char(c))));
    }
    let look = Look::m9(Lang::En, None);
    let lines: Vec<String> = pair::screen(&pairing, None, &look, COLS, ROWS)
        .iter()
        .map(row_text)
        .collect();
    assert_eq!(lines.len(), ROWS);
    assert!(lines.iter().all(|l| l.chars().count() == COLS));
    assert!(lines[0].contains("Link this M9 to your mailbox"));
    assert!(
        lines.iter().any(|l| l.contains("23 of 140 typed")),
        "{lines:#?}"
    );
    assert!(lines[3].starts_with(" lqxh1 "), "{:?}", lines[3]);
    assert!(lines[ROWS - 1].contains("Esc leaves"));
    for c in text.chars().skip(23) {
        pairing.key(&event(M9Key::Char(c)));
    }
    got = got.or(pairing.key(&event(M9Key::Enter)));
    assert_eq!(got, Some(code));
    let joining = pair::screen(&pairing, Some(Word::Joining), &look, COLS, ROWS);
    assert!(row_text(&joining[0]).contains("Joining your mailbox"));
}

/// The archive's pages become the board's chats: a message a friend sent
/// the home is a chat in the list under the friend, one the phone sent
/// through the home is ours in the same chat, a page taken twice adds
/// nothing, an entry for another address is not shown.
#[test]
fn pulled_pages_become_the_chats_list_and_a_chat() {
    let me = Identity::generate(&mut OsRng);
    let friend = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
    let entry = |seq, kind, packed| Entry {
        seq,
        kind,
        at: 1_762_000_000,
        packed,
    };
    let page = vec![
        entry(1, KIND_RECEIVED, pack(&friend, address, "hallo M9", 1.0)),
        entry(2, KIND_SENT, pack(&me, theirs, "hallo zurück", 2.0)),
        entry(3, KIND_RECEIVED, pack(&friend, theirs, "not ours", 3.0)),
    ];
    let mut model = home::model(address);
    assert!(model.conversations.is_empty());
    assert_eq!(home::take(&mut model, &page, &address), 2);
    assert_eq!(home::take(&mut model, &page[..2], &address), 2);
    assert_eq!(model.conversations.len(), 1);
    let chat = &model.conversations[0];
    assert_eq!(chat.peer, theirs);
    let bodies: Vec<(&str, bool)> = chat
        .messages
        .iter()
        .map(|m| (m.body.as_str(), m.outbound))
        .collect();
    assert_eq!(bodies, [("hallo M9", false), ("hallo zurück", true)]);
    // The list draws it; Enter opens it and the chat draws both lines.
    let list: Vec<String> = screen::lines(&screen::scene(&model), &Look::of(&model), COLS, ROWS)
        .iter()
        .map(row_text)
        .collect();
    assert!(list.iter().any(|l| l.contains("hallo zurück")), "{list:#?}");
    update(&mut model, UiEvent::Key(event(M9Key::Enter)));
    assert_ne!(model.focus, Pane::Conversations);
    let chat: Vec<String> = screen::lines(&screen::scene(&model), &Look::of(&model), COLS, ROWS)
        .iter()
        .map(row_text)
        .collect();
    assert!(chat.iter().any(|l| l.contains("hallo M9")), "{chat:#?}");
    assert!(chat.iter().any(|l| l.contains("hallo zurück")), "{chat:#?}");
}

/// A line typed in a chat on the board is the home's `/send` to the chat's
/// peer, each with an id of its own, queued oldest first; the model hears
/// how it ended.
#[test]
fn a_line_typed_on_the_board_is_a_send_to_the_home() {
    let me = Identity::generate(&mut OsRng);
    let friend = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
    let mut model = home::model(address);
    let page = vec![Entry {
        seq: 1,
        kind: KIND_RECEIVED,
        at: 1_762_000_000,
        packed: pack(&friend, address, "na?", 1.0),
    }];
    home::take(&mut model, &page, &address);
    update(&mut model, UiEvent::Key(event(M9Key::Enter)));
    // `i` writes, as in the terminal.
    update(&mut model, UiEvent::Key(event(M9Key::Char('i'))));
    assert_eq!(model.focus, Pane::Compose);
    let mut effects = Vec::new();
    for c in "vom M9".chars() {
        effects.extend(update(&mut model, UiEvent::Key(event(M9Key::Char(c)))));
    }
    effects.extend(update(&mut model, UiEvent::Key(event(M9Key::Enter))));
    let mut n = 0u8;
    let mut ids = || {
        n += 1;
        [n; 16]
    };
    let sends = home::sends(effects, &mut ids);
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(sends[0].destination, theirs);
    assert_eq!(sends[0].content, b"vom M9");
    assert_eq!(sends[0].client_id, [1; 16]);
    let mut outgoing = Outgoing::default();
    outgoing.push(sends);
    assert_eq!(outgoing.len(), 1);
    assert_eq!(
        outgoing.next().map(|s| s.content.as_slice()),
        Some(&b"vom M9"[..])
    );
    assert!(outgoing.done().is_some());
    assert!(outgoing.is_empty());
    home::sent(&mut model, Ok([9; 32]));
    // The pulled sent entry of that message names the same bubble: one
    // line on the board, not two.
    let ours = pack(&me, theirs, "vom M9", 5.0);
    let id = Message::unpack(&ours, None, None, DeliveryMethod::Direct)
        .expect("unpack")
        .message_id;
    let mut model2 = home::model(address);
    home::take(&mut model2, &page, &address);
    update(&mut model2, UiEvent::Key(event(M9Key::Enter)));
    update(&mut model2, UiEvent::Key(event(M9Key::Char('i'))));
    for c in "vom M9".chars() {
        update(&mut model2, UiEvent::Key(event(M9Key::Char(c))));
    }
    update(&mut model2, UiEvent::Key(event(M9Key::Enter)));
    home::sent(&mut model2, Ok(id));
    let sent = Entry {
        seq: 2,
        kind: KIND_SENT,
        at: 1_762_000_005,
        packed: ours,
    };
    home::take(&mut model2, &[sent], &address);
    let lines: Vec<&str> = model2.conversations[0]
        .messages
        .iter()
        .map(|m| m.body.as_str())
        .collect();
    assert_eq!(lines, ["na?", "vom M9"]);
}
