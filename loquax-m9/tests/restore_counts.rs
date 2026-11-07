//! 1514: the M9's restore is a bulk load. The board's store of 4067
//! entries restored in 59.5 s at d4b29e42 and in 1,022,384 ms at
//! 46f91e3c, because every entry the store handed back went through a
//! whole `update`, and since 1490 each `update` notes its changes over
//! every conversation, refreshes the order and rebuilds the scene's
//! window: O(conversations) per entry. Counted here, never timed: the
//! restore of a store shaped like the 1489 fixture (300 conversations,
//! 4000 messages) sorts and builds its list once, at the end.

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::board;
use loquax::home::board::store::{tag, MemFlash, Store};
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::counters::Counters;
use loquax::ui::{update, UiEvent};
use loquax_m9::home;
use loquax_m9::screen::{self, Look, COLS, ROWS};
use rand_core::OsRng;

/// As the 1489 fixture (`loquax/tests/support/real_store.rs`).
const CONVERSATIONS: usize = 300;
const MESSAGES: u64 = 4000;

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

/// Which conversation entry `seq` belongs to, shaped like the fixture:
/// one long chat and one big group's worth (1700), twenty-three friends
/// with about 40 each, the rest a few each.
fn friend_of(seq: u64) -> usize {
    let at = (seq - 1) as usize;
    match at {
        0..1700 => at % 2,
        1700..2620 => 2 + (at - 1700) % 23,
        _ => 25 + (at - 2620) % (CONVERSATIONS - 25),
    }
}

struct Archive {
    me: Identity,
    address: [u8; 16],
    friends: Vec<Identity>,
}

impl Archive {
    fn new() -> Self {
        let me = Identity::generate(&mut OsRng);
        let address = lxmf_address(&me.public_key_bytes()).expect("address");
        let friends = (0..CONVERSATIONS)
            .map(|_| Identity::generate(&mut OsRng))
            .collect();
        Self {
            me,
            address,
            friends,
        }
    }

    fn entries(&self) -> Vec<Entry> {
        (1..=MESSAGES)
            .map(|seq| {
                let friend = &self.friends[friend_of(seq)];
                let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
                let at = 1_762_000_000.0 + seq as f64;
                let (kind, packed) = match seq % 5 {
                    0 => (
                        KIND_SENT,
                        pack(&self.me, theirs, &format!("Antwort {seq}"), at),
                    ),
                    _ => (
                        KIND_RECEIVED,
                        pack(friend, self.address, &format!("Nachricht {seq}"), at),
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
}

/// The archive kept in a flash store as the pull keeps it, page by page.
fn kept(archive: &Archive) -> MemFlash {
    let mut flash = MemFlash::new(2048);
    let home::Boot {
        shelf: mut store, ..
    } = home::restore(&mut flash, archive.address).expect("boot");
    for page in archive.entries().chunks(72) {
        home::keep(&mut store, &mut flash, page, &archive.address).expect("keep");
    }
    flash
}

/// What `model` counted since `before`, its total then.
fn since(before: Counters, model: &loquax::ui::UiModel<loquax_m9::field::Field>) -> Counters {
    let now = model.total_counters().expect("this build counts");
    Counters {
        scenes: now.scenes - before.scenes,
        scene_nodes: now.scene_nodes - before.scene_nodes,
        rows_built: now.rows_built - before.rows_built,
        bubbles_built: now.bubbles_built - before.bubbles_built,
        rows_laid: now.rows_laid - before.rows_laid,
        sorts: now.sorts - before.sorts,
        sorted: now.sorted - before.sorted,
        store_rows_read: now.store_rows_read - before.store_rows_read,
        commits: now.commits - before.commits,
        walked: now.walked - before.walked,
    }
}

/// The M9's screen as text.
fn text(model: &loquax::ui::UiModel<loquax_m9::field::Field>) -> Vec<String> {
    screen::lines(&screen::scene(model), &Look::of(model), COLS, ROWS)
        .iter()
        .map(|line| line.cells.iter().map(|c| c.ch).collect())
        .collect()
}

/// The rows the list's window holds on the M9: what is on screen and as
/// many again (`UiModel::list_window`).
fn window() -> u32 {
    2 * screen::viewing().list_rows as u32
}

/// The restore the board runs at boot orders the list once and builds
/// the screen's rows once, whatever the store holds. The positive control
/// is the same entries folded one `update` each, as `home::restore` did
/// at 46f91e3c: a sort and a window of rows per entry.
#[test]
fn a_restore_sorts_and_builds_once() {
    if !loquax::ui::counters::ON {
        println!("NOT COUNTED: a release build without loquax's counters feature");
        return;
    }
    let archive = Archive::new();
    let mut flash = kept(&archive);

    let home::Boot {
        model, restored, ..
    } = home::restore(&mut flash, archive.address).expect("restore");
    assert_eq!(restored.entries, MESSAGES);
    assert_eq!(model.conversations.len(), CONVERSATIONS);
    // The whole restore's count, every event in it: a model's count of
    // its last event alone would see one entry of the per-entry fold.
    // From what an empty board's model counted being made.
    let made = home::model(archive.address)
        .total_counters()
        .expect("this build counts");
    let counts = since(made, &model);
    println!("COUNTS m9.restore {counts}");
    assert!(
        counts.sorts <= 1 && counts.rows_built <= window() && counts.scenes == 0,
        "a restore of {MESSAGES} entries: {counts} (bar sorts 1, rows_built {})",
        window()
    );
    // 1521: the batch's folds found every id by the summary's index, as
    // the per-entry folds did; before, the summary stood behind until the
    // note and each entry walked its conversation (2,194,736 messages).
    assert_eq!(counts.walked, 0, "a restore walked: {counts}");

    // The control: per entry, the cost is a sort and a window each.
    // Folded with no chat open, as the restore folds (1493): the boot
    // reads nothing "where it arrived".
    let mut each = home::model(archive.address);
    each.selected = loquax::ui::NONE_OPEN;
    Store::open(&mut flash, tag(&archive.address), |entry| {
        if let Some(event) = board::fold(&entry, &archive.address) {
            update(&mut each, UiEvent::Engine(event));
        }
    })
    .expect("open");
    each.selected = each.order()[0];
    let sum = each.total_counters().expect("this build counts");
    println!("COUNTS m9.restore_per_entry {sum}");
    assert!(
        sum.sorts as u64 >= MESSAGES / 2,
        "the control sorted {} times",
        sum.sorts
    );
    assert_eq!(
        text(&each),
        text(&model),
        "the batch shows what the entries one by one show"
    );
}

/// After the restore, one new message (a pulled page of one entry, the
/// board's turn) moves one conversation in the order and builds the
/// screen's window, never the store.
#[test]
fn a_new_message_moves_one_conversation() {
    if !loquax::ui::counters::ON {
        return;
    }
    let archive = Archive::new();
    let mut flash = kept(&archive);
    let home::Boot { mut model, .. } = home::restore(&mut flash, archive.address).expect("restore");
    let friend = &archive.friends[CONVERSATIONS - 1];
    let entry = Entry {
        seq: MESSAGES + 1,
        kind: KIND_RECEIVED,
        at: 1_762_100_000,
        packed: pack(friend, archive.address, "Bist du da?", 1_762_100_000.0),
    };
    let before = model.total_counters().expect("this build counts");
    assert_eq!(home::take(&mut model, &[entry], &archive.address).0, 1);
    let counts = since(before, &model);
    println!("COUNTS m9.one_new_message {counts}");
    assert!(
        counts.sorts <= 1 && counts.sorted <= 1 && counts.rows_built <= window(),
        "one new message: {counts} (bar sorts 1, sorted 1, rows_built {})",
        window()
    );
    let top = &model.conversations[model.order()[0]];
    assert_eq!(
        top.peer,
        lxmf_address(&friend.public_key_bytes()).expect("address"),
        "the conversation with the new message leads the list"
    );
}

/// A pulled page of 72 entries, the board's first page, orders and
/// builds once, as a restore does: the `take=594` half of the 1387 ms
/// turn on the board.
#[test]
fn a_pulled_page_sorts_and_builds_once() {
    if !loquax::ui::counters::ON {
        return;
    }
    let archive = Archive::new();
    let all = archive.entries();
    let (page, rest) = all.split_at(all.len() - 72);
    let mut model = home::model(archive.address);
    home::take(&mut model, page, &archive.address);
    let before = model.total_counters().expect("this build counts");
    assert_eq!(home::take(&mut model, rest, &archive.address).0, 72);
    let counts = since(before, &model);
    println!("COUNTS m9.pulled_page {counts}");
    assert!(
        counts.sorts <= 1 && counts.rows_built <= window(),
        "a page of 72: {counts} (bar sorts 1, rows_built {})",
        window()
    );
}

/// The fixture kept, then booted once: the boot that finds no summary
/// folds the ring and writes the summary log from it (1521).
fn summarised(archive: &Archive) -> MemFlash {
    let mut flash = kept(archive);
    let boot = home::restore(&mut flash, archive.address).expect("first boot");
    assert!(
        matches!(boot.how, home::How::Rebuilt { .. }),
        "a store from before the summaries is read whole once: {:?}",
        boot.how
    );
    flash
}

/// 1521: the boot after it lists the 300 conversations from their
/// summaries alone: no entry decoded, the list ordered once and its
/// window built once, and the list the reader sees is the one the whole
/// fold showed.
#[test]
fn a_boot_from_summaries_decodes_no_entry() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let folded = home::restore(&mut flash.clone(), archive.address).expect("control");
    // The control is the whole fold of the same ring: the summary log
    // erased, the ring as it was.
    let mut erased = flash.clone();
    let (ring, _) = loquax::home::board::rows::split(2048).expect("split");
    for sector in ring..2048 {
        erased.sectors[sector as usize].fill(0xFF);
    }
    let whole = home::restore(&mut erased, archive.address).expect("whole");
    assert!(matches!(whole.how, home::How::Rebuilt { why: "no-state" }));

    let started = std::time::Instant::now();
    let boot = home::restore(&mut flash, archive.address).expect("resume");
    let took = started.elapsed();
    println!("TIME m9.resume {took:?} (records {:?})", boot.how);
    assert!(
        matches!(boot.how, home::How::Resumed { .. }),
        "{:?}",
        boot.how
    );
    assert!(matches!(folded.how, home::How::Resumed { .. }));
    assert_eq!(boot.decoded, 0, "a resumed boot decodes no entry");
    assert_eq!(boot.restored.entries, MESSAGES);
    assert_eq!(boot.shelf.cursor(), MESSAGES);
    assert_eq!(boot.model.conversations.len(), CONVERSATIONS);
    assert!(boot
        .model
        .conversations
        .iter()
        .all(|c| c.messages.is_empty()));
    // Counted before anything draws: the restore's own work.
    if loquax::ui::counters::ON {
        let made = home::model(archive.address)
            .total_counters()
            .expect("this build counts");
        let counts = since(made, &boot.model);
        println!("COUNTS m9.resume {counts}");
        assert!(
            counts.sorts == 1 && counts.rows_built <= window() && counts.walked == 0,
            "a boot from summaries: {counts} (bar sorts 1, rows_built {})",
            window()
        );
    }
    assert_eq!(
        text(&boot.model),
        text(&whole.model),
        "the list from summaries is the list the whole fold shows"
    );
}

/// 1521: a conversation that opens reads its newest page from its own
/// sectors: only its entries, the newest of them, folded into the
/// bubbles the whole fold made of them; the store's older ones wait.
#[test]
fn a_conversation_opens_with_its_newest_page_alone() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let mut erased = flash.clone();
    let (ring, _) = loquax::home::board::rows::split(2048).expect("split");
    for sector in ring..2048 {
        erased.sectors[sector as usize].fill(0xFF);
    }
    let whole = home::restore(&mut erased, archive.address).expect("whole");
    let mut boot = home::restore(&mut flash, archive.address).expect("resume");
    assert!(matches!(boot.how, home::How::Resumed { .. }));

    // The long chat: friend 0, every other entry of the first 1700.
    let peer = lxmf_address(&archive.friends[0].public_key_bytes()).expect("address");
    let ours: Vec<u64> = (1..=MESSAGES).filter(|&seq| friend_of(seq) == 0).collect();
    let want = home::page_entries();
    let (entries, earlier) = boot.shelf.page(&mut flash, peer, want).expect("page");
    let seqs: Vec<u64> = entries
        .iter()
        .filter(|e| e.kind != loquax::home::KIND_MARK)
        .map(|e| e.seq)
        .collect();
    assert_eq!(
        seqs,
        ours[ours.len() - want..],
        "the page is the conversation's newest {want} entries and nothing else"
    );
    assert_eq!(earlier, ours.len() - want);

    // Selected, the model asks the page; the answer lands it.
    let index = boot
        .model
        .conversations
        .iter()
        .position(|c| c.peer == peer)
        .expect("listed");
    boot.model.selected = index;
    let effects = home::page_in(&mut boot.model, peer, &entries, earlier, &archive.address);
    let mut ids = || [0u8; 16];
    let routed = home::route(&mut boot.model, effects, &mut ids);
    assert!(routed.unrouted.is_empty(), "{:?}", routed.unrouted);
    let loaded = &boot.model.conversations[index];
    let full = whole
        .model
        .conversations
        .iter()
        .find(|c| c.peer == peer)
        .expect("in the whole fold");
    let bodies = |messages: &[loquax::ui::Message]| -> Vec<String> {
        messages.iter().map(|m| m.body.clone()).collect()
    };
    assert_eq!(
        bodies(&loaded.messages),
        bodies(&full.messages[full.messages.len() - want..]),
        "the page's bubbles are the whole fold's newest"
    );
    assert_eq!(loaded.earlier, ours.len() - want);

    // The next page is the one before, never the same again.
    let (older, _) = boot.shelf.page(&mut flash, peer, want).expect("older");
    let older: Vec<u64> = older
        .iter()
        .filter(|e| e.kind != loquax::home::KIND_MARK)
        .map(|e| e.seq)
        .collect();
    assert_eq!(older, ours[ours.len() - 2 * want..ours.len() - want]);
}

/// 1521, the unhappy path: a page kept whose summary never got written
/// (the power went between the two) leaves the ring past the summary's
/// end; the boot reads the ring whole, shows the page, and the boot
/// after resumes again.
#[test]
fn a_ring_past_its_summary_is_read_whole_once() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let boot = home::restore(&mut flash, archive.address).expect("resume");
    let mut shelf = boot.shelf;
    let friend = &archive.friends[7];
    let entry = Entry {
        seq: MESSAGES + 1,
        kind: KIND_RECEIVED,
        at: 1_762_100_000,
        packed: pack(
            friend,
            archive.address,
            "nach dem Stromausfall",
            1_762_100_000.0,
        ),
    };
    home::keep(&mut shelf, &mut flash, &[entry], &archive.address).expect("keep");
    // No flush: the power went.
    let after = home::restore(&mut flash, archive.address).expect("after the cut");
    assert_eq!(after.how, home::How::Rebuilt { why: "ring-moved" });
    assert_eq!(after.shelf.cursor(), MESSAGES + 1);
    let peer = lxmf_address(&friend.public_key_bytes()).expect("address");
    let top = &after.model.conversations[after.model.order()[0]];
    assert_eq!(top.peer, peer, "the page the summary missed is shown");
    let again = home::restore(&mut flash, archive.address).expect("again");
    assert!(
        matches!(again.how, home::How::Resumed { .. }),
        "{:?}",
        again.how
    );
    let top = &again.model.conversations[again.model.order()[0]];
    assert_eq!(top.peer, peer);
}

/// 1521: a page folded live, its rows said and flushed, comes back from
/// the summaries after a reboot: the conversation leads, its unread
/// counted, and its newest message is the page's; a read mark from
/// another device on a conversation none of whose messages is loaded
/// clears its badge, and that survives the reboot too.
#[test]
fn live_pages_and_marks_survive_a_resumed_boot() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let mut boot = home::restore(&mut flash, archive.address).expect("resume");
    let friend = &archive.friends[CONVERSATIONS - 1];
    let peer = lxmf_address(&friend.public_key_bytes()).expect("address");
    let entry = Entry {
        seq: MESSAGES + 1,
        kind: KIND_RECEIVED,
        at: 1_762_100_000,
        packed: pack(friend, archive.address, "Bist du da?", 1_762_100_000.0),
    };
    home::keep(
        &mut boot.shelf,
        &mut flash,
        std::slice::from_ref(&entry),
        &archive.address,
    )
    .expect("keep");
    boot.shelf.pages_owed += 1;
    let (_, effects) = home::take(&mut boot.model, &[entry], &archive.address);
    let mut ids = || [0u8; 16];
    let routed = home::route(&mut boot.model, effects, &mut ids);
    assert!(
        routed.rows.iter().any(|row| row.peer == peer),
        "the fold says the conversation's row"
    );
    boot.shelf.said(routed.rows, routed.gone);
    boot.shelf.pages_owed -= 1;
    boot.shelf.flush(&mut flash, Vec::new).expect("flush");

    let again = home::restore(&mut flash, archive.address).expect("reboot");
    assert!(
        matches!(again.how, home::How::Resumed { .. }),
        "{:?}",
        again.how
    );
    let top = &again.model.conversations[again.model.order()[0]];
    assert_eq!(top.peer, peer, "the live page's conversation leads");
    let unread = top.unread;
    assert!(unread >= 1);

    // Another device read it: the mark arrives as a pulled entry.
    let mut again = again;
    let mark_entry = Entry {
        seq: MESSAGES + 2,
        kind: loquax::home::KIND_MARK,
        at: 0,
        packed: loquax::home::wire::mark_request(&loquax::home::wire::Mark {
            flag: loquax::home::wire::MarkFlag::Read,
            ids: vec![newest_id(&flash, &archive, peer)],
        }),
    };
    home::keep(
        &mut again.shelf,
        &mut flash,
        std::slice::from_ref(&mark_entry),
        &archive.address,
    )
    .expect("keep mark");
    let (_, effects) = home::take(&mut again.model, &[mark_entry], &archive.address);
    let routed = home::route(&mut again.model, effects, &mut ids);
    let row = routed
        .rows
        .iter()
        .find(|row| row.peer == peer)
        .expect("the mark changes the row");
    assert_eq!(row.unread, 0, "the read mark clears the unloaded badge");
    again.shelf.said(routed.rows, routed.gone);
    again.shelf.flush(&mut flash, Vec::new).expect("flush");
    let third = home::restore(&mut flash, archive.address).expect("third");
    assert!(matches!(third.how, home::How::Resumed { .. }));
    let conversation = third
        .model
        .conversations
        .iter()
        .find(|c| c.peer == peer)
        .expect("listed");
    assert_eq!(conversation.unread, 0, "read stays read after the reboot");
}

/// The id of conversation `peer`'s newest received message, read back
/// from the ring as a whole fold reads it.
fn newest_id(flash: &MemFlash, archive: &Archive, peer: [u8; 16]) -> [u8; 32] {
    let mut copy = flash.clone();
    let (ring, _) = loquax::home::board::rows::split(2048).expect("split");
    for sector in ring..2048 {
        copy.sectors[sector as usize].fill(0xFF);
    }
    let whole = home::restore(&mut copy, archive.address).expect("whole");
    whole
        .model
        .conversations
        .iter()
        .find(|c| c.peer == peer)
        .and_then(|c| c.messages.iter().rev().find(|m| !m.outbound))
        .and_then(|m| m.id)
        .expect("a received message")
}
