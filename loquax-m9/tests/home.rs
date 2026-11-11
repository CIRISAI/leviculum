//! The M9 as a device of its home, on the screen's side (1415): the word
//! in the top bar, the pairing screen the code is typed on, the archive's
//! pages becoming the chats list and a chat, and a line typed on the board
//! becoming the home's `/send`.

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use leviculum_ui::style::RLine;
use loquax::home::ask::Verb;
use loquax::home::board::Word;
use loquax::home::code::HomeCode;
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::strings::Lang;
use loquax::ui::{update, Pane, UiEvent};
use loquax_m9::group;
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

/// The word stands at the bar's right end, one short word whole (1564):
/// "Chats" and "Verbunden", never "90 · Chats · Du. Dein Postfach ist…"
/// (Lew at the M9, 2026-11-08). No avatar of the reader's, no count, no
/// sentence, no word cut; the bar is the grid's width whatever the word;
/// without a home the bar says the page alone.
#[test]
fn the_top_bar_says_the_page_and_one_word() {
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
    let none = bar(None, Lang::De);
    assert_eq!(none.trim(), "Chats", "{none:?}");
    let fetched = Word::Fetched {
        taken: 12,
        of: Some(40),
    };
    for (word, en, de) in [
        (Word::Joining, "Connecting…", "Verbindet…"),
        (Word::Unreachable, "Disconnected", "Getrennt"),
        (fetched, "Fetching…", "Holt ab…"),
        (Word::Connected, "Connected", "Verbunden"),
        (Word::Refused, "Refused", "Abgelehnt"),
        (Word::CodeRefused, "Refused", "Abgelehnt"),
    ] {
        for (lang, said) in [(Lang::En, en), (Lang::De, de)] {
            let line = bar(Some(word), lang);
            assert_eq!(line.chars().count(), COLS, "{line:?}");
            let rest = line.trim().strip_prefix("Chats").map(str::trim);
            assert_eq!(rest, Some(said), "{line:?}");
        }
    }
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
    assert!(lines[ROWS - 1].contains("Back leaves"));
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
    assert_eq!(home::take(&mut model, &page, &address).0, 2);
    assert_eq!(home::take(&mut model, &page[..2], &address).0, 2);
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
    let routed = home::route(&mut model, effects, &mut ids);
    let sends: Vec<&loquax::home::wire::Submission> = routed
        .verbs
        .iter()
        .filter_map(|verb| match verb {
            Verb::Send(submission) => Some(submission),
            _ => None,
        })
        .collect();
    assert_eq!(sends.len(), 1, "{:?}", routed.verbs);
    assert_eq!(sends[0].destination, theirs);
    assert_eq!(sends[0].content, b"vom M9");
    let mut outgoing = Outgoing::default();
    for verb in routed.verbs {
        if matches!(verb, Verb::Send(_)) {
            outgoing.push([0x11; 16], verb);
        }
    }
    assert_eq!(outgoing.len(), 1);
    assert!(matches!(
        outgoing.next(),
        Some(([0x11, ..], Verb::Send(s))) if s.content == b"vom M9"
    ));
    assert!(outgoing.done().is_some());
    assert!(outgoing.is_empty());
    home::answered(&mut model, home::Answer::Sent(Ok([9; 32])));
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
    home::answered(&mut model2, home::Answer::Sent(Ok(id)));
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

/// A pulled archive of `n` messages from `friends` to `address`, oldest
/// first, one in five sent by the board's own home (1476).
fn archive(me: &Identity, address: [u8; 16], friends: &[Identity], n: u64) -> Vec<Entry> {
    (1..=n)
        .map(|seq| {
            let friend = &friends[seq as usize % friends.len()];
            let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
            let at = 1_762_000_000.0 + seq as f64;
            let (kind, packed) = match seq % 5 {
                0 => (KIND_SENT, pack(me, theirs, &format!("Antwort {seq}"), at)),
                _ => (
                    KIND_RECEIVED,
                    pack(friend, address, &format!("Nachricht {seq}"), at),
                ),
            };
            Entry {
                seq,
                kind,
                at: at as i64,
                packed,
            }
        })
        .collect()
}

fn list_text(model: &loquax::ui::UiModel<loquax_m9::field::Field>) -> Vec<String> {
    screen::lines(&screen::scene(model), &Look::of(model), COLS, ROWS)
        .iter()
        .map(row_text)
        .collect()
}

/// 1476, the photo of 2026-11-07: the board pulls its archive in pages,
/// each kept in the flash store as it comes, and is "rebooted": the PSRAM
/// model is dropped, the flash images (the client record and the store)
/// stay. At boot the store gives back the same chats list, every message
/// of every chat, and the record's cursor agrees with the store, so the
/// next pull asks for nothing it has. The control: a board whose store is
/// gone (the flash erased) shows the empty list and its cursor resets.
#[test]
fn the_chats_come_back_from_the_flash_store_after_a_reboot() {
    use loquax::home::board::store::MemFlash;
    use loquax::home::board::{agree, Agreement};
    use loquax::home::record::{Admission, ClientRecord};

    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..3).map(|_| Identity::generate(&mut OsRng)).collect();
    let all = archive(&me, address, &friends, 120);
    let code = HomeCode {
        public_key: me.public_key_bytes(),
        token: [3; 16],
    };
    let device = Identity::generate(&mut OsRng);
    let mut record = ClientRecord::pending(&device, &code, "M9").expect("record");
    record.admission = Admission::Admitted;

    // The pull: pages of 25, each into the model and the store.
    let mut flash = MemFlash::new(64);
    let home::Boot {
        mut model,
        shelf: mut store,
        restored,
        ..
    } = home::restore(&mut flash, address).expect("boot");
    assert_eq!(restored.entries, 0);
    let mut writes = 0;
    for page in all.chunks(25) {
        home::take(&mut model, page, &address);
        let done = home::keep(&mut store, &mut flash, page, &address).expect("keep");
        assert_eq!(done.entries, page.len() as u64);
        writes += done.writes;
        record.cursor = page.last().map(|e| e.seq).unwrap_or(0);
    }
    // A write per page, and one more each time a page runs into the
    // next sector.
    assert!(
        writes <= 5 + store.used(),
        "{writes} writes for 5 pages over {} sectors",
        store.used()
    );
    let before = list_text(&model);
    assert!(
        before.iter().any(|l| l.contains("Nachricht")),
        "{before:#?}"
    );
    let chats: Vec<_> = model
        .conversations
        .iter()
        .map(|c| (c.peer, c.messages.len()))
        .collect();
    assert_eq!(chats.len(), 3);

    // The reboot: RAM gone (the model; the store's state is rebuilt from
    // flash below), flash kept.
    drop(model);
    let flash_record = record.encode();
    let mut booted = ClientRecord::decode(&flash_record)
        .expect("record")
        .expect("some");
    let home::Boot {
        model,
        shelf: store,
        restored,
        ..
    } = home::restore(&mut flash, address).expect("reboot");
    assert_eq!(restored.entries, 120);
    assert_eq!(agree(&mut booted, store.cursor()), Agreement::Agrees);
    assert_eq!(booted.cursor, 120);
    // The same list; the cursor stands on its first row after a boot
    // (1521), wherever the pull left it.
    let bare = |lines: Vec<String>| -> Vec<String> {
        lines.into_iter().map(|l| l.replace('▌', " ")).collect()
    };
    assert_eq!(
        bare(list_text(&model)),
        bare(before.clone()),
        "the same list as before"
    );
    let again: Vec<_> = model
        .conversations
        .iter()
        .map(|c| (c.peer, c.messages.len()))
        .collect();
    assert_eq!(again, chats);

    // The control: the store erased, the list is empty and the cursor
    // resets, so the next pull fills it.
    let mut erased = MemFlash::new(64);
    let home::Boot {
        model,
        shelf: store,
        ..
    } = home::restore(&mut erased, address).expect("erased");
    assert!(model.conversations.is_empty());
    let mut lost = ClientRecord::decode(&flash_record)
        .expect("record")
        .expect("some");
    assert_eq!(
        agree(&mut lost, store.cursor()),
        Agreement::Lost { was: 120 }
    );
    assert_eq!(lost.cursor, 0);
}

/// 1603, window 22: after a rebuild the summary log was written one
/// bounded write a quiet turn (1597), 30-odd writes a second apart, its
/// state last; the walks rebooted within those seconds, so every boot
/// found no state and rebuilt again for 25 s (`why=no-state`, three boots
/// in a row). The rebuild now writes the whole summary before the boot
/// goes on (in bounded writes, the caller's clock between them, before
/// the UI core takes the model): a reboot right after a rebuild resumes,
/// with the same list.
#[test]
fn a_reboot_right_after_a_rebuild_resumes_with_the_same_list() {
    use loquax::home::board::rows;
    use loquax::home::board::store::{MemFlash, Sectors};

    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..300).map(|_| Identity::generate(&mut OsRng)).collect();
    let all = archive(&me, address, &friends, 2400);
    let mut flash = MemFlash::new(1024);
    let home::Boot {
        shelf: mut store, ..
    } = home::restore(&mut flash, address).expect("boot");
    for page in all.chunks(72) {
        home::keep(&mut store, &mut flash, page, &address).expect("keep");
    }
    drop(store);
    // The summary erased, as `bench forget` leaves it.
    let (ring, summary) = rows::split(1024).expect("split");
    for sector in ring..ring + summary {
        flash.erase(sector).expect("erase");
    }

    let rebuilt = home::restore(&mut flash, address).expect("rebuild");
    assert_eq!(rebuilt.how.word(), "rebuilt");
    assert_eq!(rebuilt.model.conversations.len(), 300);
    let before = list_text(&rebuilt.model);
    assert!(
        !rebuilt.shelf.owes(),
        "the rebuild left the summary owed: a reset now rebuilds again"
    );
    // The reset: nothing in memory, no quiet turn between.
    drop(rebuilt);
    let resumed = home::restore(&mut flash, address).expect("reboot");
    assert_eq!(
        resumed.how.word(),
        "resumed",
        "a reboot right after a rebuild boot rebuilt again"
    );
    assert_eq!(resumed.decoded, 0, "the resumed boot read no entry");
    assert_eq!(resumed.model.conversations.len(), 300);
    let bare = |lines: Vec<String>| -> Vec<String> {
        lines.into_iter().map(|l| l.replace('▌', " ")).collect()
    };
    assert_eq!(
        bare(list_text(&resumed.model)),
        bare(before),
        "the same list"
    );
}

/// 1476, the fold decides what is stored: a message for this board and
/// one it sent are kept, a message for another address, a delivery state
/// and an imported entry are not; the page's end still moves the store's
/// cursor past them all, so they are not pulled again.
#[test]
fn the_store_keeps_what_the_fold_keeps() {
    use loquax::home::board::store::MemFlash;
    use loquax::home::{KIND_DELIVERED, KIND_IMPORTED};

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
        entry(1, KIND_RECEIVED, pack(&friend, address, "für dich", 1.0)),
        entry(2, KIND_SENT, pack(&me, theirs, "von mir", 2.0)),
        entry(3, KIND_RECEIVED, pack(&friend, theirs, "nicht unsere", 3.0)),
        entry(4, KIND_DELIVERED, vec![7; 32]),
        entry(5, KIND_IMPORTED, pack(&friend, address, "importiert", 4.0)),
    ];
    let mut flash = MemFlash::new(8);
    let home::Boot {
        shelf: mut store, ..
    } = home::restore(&mut flash, address).expect("boot");
    let done = home::keep(&mut store, &mut flash, &page, &address).expect("keep");
    assert_eq!(done.entries, 2);
    let mut kept = Vec::new();
    let (opened, _, _) = loquax::home::board::store::Store::open(
        &mut flash,
        loquax::home::board::store::tag(&address),
        |e| kept.push(e.seq),
    )
    .expect("open");
    assert_eq!(kept, [1, 2]);
    assert_eq!(opened.cursor(), 5, "the page's end, past what was not kept");
    // An empty page writes nothing.
    let writes = flash.writes;
    let done = home::keep(&mut store, &mut flash, &[], &address).expect("empty");
    assert_eq!(done, Default::default());
    assert_eq!(flash.writes, writes);
}

/// 1476, the report's number: the restore of 4058 entries the size of
/// Lew's archive of 2026-11-06 (a text with its signature), timed on the
/// host; the board says its own time in its `[STORE]` line.
#[test]
fn a_restore_of_4058_entries_is_timed() {
    use loquax::home::board::store::{MemFlash, SECTOR};
    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..12).map(|_| Identity::generate(&mut OsRng)).collect();
    let all = archive(&me, address, &friends, 4058);
    let bytes: usize = all.iter().map(|e| e.packed.len()).sum();
    let mut flash = MemFlash::new(1024);
    let home::Boot {
        shelf: mut store, ..
    } = home::restore(&mut flash, address).expect("boot");
    for page in all.chunks(67) {
        home::keep(&mut store, &mut flash, page, &address).expect("keep");
    }
    let used = store.used();
    let start = std::time::Instant::now();
    let home::Boot {
        model,
        shelf: store,
        restored,
        ..
    } = home::restore(&mut flash, address).expect("restore");
    let took = start.elapsed();
    assert_eq!(restored.entries, 4058);
    assert_eq!(store.cursor(), 4058);
    assert_eq!(model.conversations.len(), 12);
    println!(
        "[1476] restore entries=4058 packed_bytes={bytes} sectors_used={used} flash_bytes={} host_ms={}",
        used as usize * SECTOR,
        took.as_millis()
    );
}

/// 1493, the effect router: every verb of the home protocol the model
/// asks for reaches the ask queue. A group made on the M9 becomes
/// `/group` (the cell behind "a group made on the M9 appears on the
/// phone": the ask is what the home fans out), members become
/// `/groupadd`, a delete `/groupdelete`, and a read mark `/mark` — none
/// of them is dropped on the floor as `home::take` dropped them before.
#[test]
fn every_group_verb_and_mark_reaches_the_ask_queue() {
    use loquax::home::ask::GroupVerb;
    use loquax::home::wire::{Mark, MarkFlag};
    use loquax::ui::UiEffect;

    let me = Identity::generate(&mut OsRng);
    let friend = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let mut model = home::model(address);
    let mut n = 0u8;
    let mut ids = || {
        n += 1;
        [n; 16]
    };
    // The mark comes the reader's way: a message arrives, the chat is
    // opened, the model queues the read mark (`read_out`; it queued
    // nothing before 1493, when the board was no `home_client`).
    let packed = pack(&friend, address, "gelesen gleich", 1.0);
    let id = Message::unpack(&packed, None, None, DeliveryMethod::Direct)
        .expect("unpack")
        .message_id;
    let page = vec![Entry {
        seq: 1,
        kind: KIND_RECEIVED,
        at: 1_762_000_000,
        packed,
    }];
    home::take(&mut model, &page, &address);
    let mut row = 0;
    let opened = group::press(&mut model, &mut row, &event(M9Key::Enter));
    assert!(opened.is_empty(), "{opened:?}");
    let effects = vec![
        UiEffect::CreateGroup {
            name: "Band".into(),
            members: vec![([4; 16], "Anna".into())],
        },
        UiEffect::AddMembers {
            group: [6; 16],
            members: vec![([7; 16], "Ben".into())],
        },
        UiEffect::DeleteGroup { group: [6; 16] },
    ];
    let routed = home::route(&mut model, effects, &mut ids);
    let kinds: Vec<&str> = routed
        .verbs
        .iter()
        .map(|verb| match verb {
            Verb::Mark(_) => "mark",
            Verb::Group(GroupVerb::Create(_)) => "group",
            Verb::Group(GroupVerb::Add(_)) => "groupadd",
            Verb::Group(GroupVerb::Delete(_)) => "groupdelete",
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["mark", "group", "groupadd", "groupdelete"]);
    assert_eq!(
        routed.marks,
        [Mark {
            flag: MarkFlag::Read,
            ids: vec![id],
        }]
    );
    // The queues the desktop drains each fold are drained here too: a
    // second route hands out nothing again.
    let routed = home::route(&mut model, Vec::new(), &mut ids);
    assert!(routed.verbs.is_empty() && routed.marks.is_empty());
}

/// 1493: read on the board is read after a reboot, home heard from or
/// not. The board's own mark is kept in the message store and folds back
/// at restore exactly as a pulled mark folds. The control: without
/// `keep_marks` the reboot shows the message unread again (the finding).
#[test]
fn read_on_the_board_survives_a_reboot() {
    use loquax::home::board::store::MemFlash;

    let me = Identity::generate(&mut OsRng);
    let friend = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let page = vec![Entry {
        seq: 1,
        kind: KIND_RECEIVED,
        at: 1_762_000_000,
        packed: pack(&friend, address, "ungelesen?", 1.0),
    }];

    let mut flash = MemFlash::new(16);
    let home::Boot {
        mut model,
        shelf: mut store,
        ..
    } = home::restore(&mut flash, address).expect("boot");
    home::take(&mut model, &page, &address);
    home::keep(&mut store, &mut flash, &page, &address).expect("keep");
    assert_eq!(model.conversations[0].unread_rows(), 1);

    // The control first: a reboot without the kept mark is the finding.
    let home::Boot { model: unfixed, .. } =
        home::restore(&mut flash, address).expect("control reboot");
    assert_eq!(
        unfixed.conversations[0].unread_rows(),
        1,
        "without keep_marks the reboot forgets the read"
    );

    // The board reads it: the fold's own mark goes into the store.
    let mut row = 0;
    let effects = group::press(&mut model, &mut row, &event(M9Key::Enter));
    let routed = home::route(&mut model, effects, &mut || [1; 16]);
    assert_eq!(routed.marks.len(), 1, "the open chat marks its unread read");
    home::keep_marks(&mut store, &mut flash, &routed.marks).expect("kept");
    let cursor = store.cursor();
    let home::Boot {
        model,
        shelf: store,
        ..
    } = home::restore(&mut flash, address).expect("reboot");
    assert_eq!(model.conversations[0].unread_rows(), 0, "read stays read");
    assert_eq!(store.cursor(), cursor, "a local mark moves no cursor");
}

/// 1493: an ask queued when the power goes is asked again at the next
/// boot — the send still reaches the home.
#[test]
fn a_queued_send_survives_a_reboot() {
    use loquax::home::board::store::MemFlash;
    use loquax::home::board::{ask_bytes, ask_verb};
    use loquax::home::wire::Submission;

    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let mut flash = MemFlash::new(16);
    let home::Boot {
        shelf: mut store,
        asks: pending,
        ..
    } = home::restore(&mut flash, address).expect("boot");
    assert!(pending.is_empty());
    let send = Verb::Send(Submission {
        client_id: [0xAA; 16],
        destination: [2; 16],
        title: Vec::new(),
        content: b"vom M9".to_vec(),
        fields: Vec::new(),
    });
    let (tag, bytes) = ask_bytes(&send).expect("persistable");
    store
        .keep_ask(&mut flash, &[0xAA; 16], tag, &bytes)
        .expect("kept");

    // The power goes; the boot queues it again.
    let home::Boot {
        shelf: mut store,
        asks: pending,
        ..
    } = home::restore(&mut flash, address).expect("reboot");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, [0xAA; 16]);
    assert_eq!(ask_verb(pending[0].tag, &pending[0].request), Some(send));

    // Answered and marked done: the next boot owes nothing.
    store.ask_done(&mut flash, &[0xAA; 16]).expect("done");
    let home::Boot { asks: pending, .. } = home::restore(&mut flash, address).expect("third boot");
    assert!(pending.is_empty());
}

/// 1493: a write that did not land is a word on the screen. The fault
/// takes the bar's right end, over the home's word.
#[test]
fn a_fault_is_a_word_in_the_top_bar() {
    let me = Identity::generate(&mut OsRng);
    let model = home::model(lxmf_address(&me.public_key_bytes()).expect("address"));
    let scene = screen::scene(&model);
    let look = Look {
        fault: Some(loquax::ui::strings::StrId::M9NotSaved),
        lang: Lang::En,
        ..Look::of(&model)
    };
    let lines = screen::lines(&scene, &look, COLS, ROWS);
    let top = row_text(&lines[0]);
    // The state in its short word (1564), the sentence is the log's.
    assert!(top.trim_end().ends_with("Not saved"), "{top:?}");
    assert!(!top.contains("memory"), "{top:?}");
    let clear = Look {
        fault: None,
        lang: Lang::En,
        ..Look::of(&model)
    };
    let top = row_text(&screen::lines(&scene, &clear, COLS, ROWS)[0]);
    assert!(!top.contains("Not saved"), "{top:?}");
}
