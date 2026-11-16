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
use loquax::home::board::rows::ERASE_BUDGET;
use loquax::home::board::store::{tag, MemFlash, Sectors, Store};
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
        self.entries_ahead(0.0)
    }

    /// [`Archive::entries`] with the friends' clocks `ahead` seconds ahead
    /// of the home that recorded their messages (1594): each received
    /// message says it was written later than the home took it; our own
    /// answers go by the home's clock.
    fn entries_ahead(&self, ahead: f64) -> Vec<Entry> {
        (1..=MESSAGES)
            .map(|seq| {
                let friend = &self.friends[friend_of(seq)];
                let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
                let at = 1_762_000_000.0 + seq as f64;
                let written = at + ahead;
                let (kind, packed) = match seq % 5 {
                    // Our own answers by the home's clock.
                    0 => (
                        KIND_SENT,
                        pack(&self.me, theirs, &format!("Antwort {seq}"), at),
                    ),
                    _ => (
                        KIND_RECEIVED,
                        pack(friend, self.address, &format!("Nachricht {seq}"), written),
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

/// The quiet turns after a write of the summary log (1597): every
/// bounded write the shelf owes, made, as the board's loop makes them
/// one a quiet turn. A boot writes one; the next boot resumes only once
/// the rest went.
fn settle(
    shelf: &mut home::Shelf,
    flash: &mut MemFlash,
    asks: Vec<loquax::home::board::store::PendingAsk>,
) {
    let mut calls = 0;
    loop {
        let done = shelf.flush(flash, || asks.clone()).expect("flush");
        assert!(
            done.appended().erased <= ERASE_BUDGET,
            "a write erased {}",
            done.appended().erased
        );
        if !done.partial() && !shelf.owes() {
            break;
        }
        calls += 1;
        assert!(calls < 10_000, "the summary log never settled");
    }
}

/// [`home::restore`], and the quiet turns after it ([`settle`]).
fn restore_settled(
    flash: &mut MemFlash,
    address: [u8; 16],
) -> Result<home::Boot, <MemFlash as Sectors>::Error> {
    let mut boot = home::restore(flash, address)?;
    let asks = boot.asks.clone();
    settle(&mut boot.shelf, flash, asks);
    Ok(boot)
}

/// The archive kept in a flash store as the pull keeps it, page by page.
fn kept(archive: &Archive) -> MemFlash {
    let mut flash = MemFlash::new(2048);
    let home::Boot {
        shelf: mut store, ..
    } = restore_settled(&mut flash, archive.address).expect("boot");
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
        copied: now.copied - before.copied,
        probed: now.probed - before.probed,
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
    2 * screen::viewing(screen::Size::DEFAULT).list_rows as u32
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
    } = restore_settled(&mut flash, archive.address).expect("restore");
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
    // 1521b: the boot drains what the fold queued for a store without
    // copying a message: window 7's restore cloned all 4068 into store
    // writes and dropped them, 56 s under the board's first-fit heap.
    assert_eq!(counts.copied, 0, "a restore copied: {counts}");

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
    // The control for `copied` (1521b): the desktop's drain, which the
    // board used until 1521b, copies every message the fold changed.
    let before = each.total_counters().expect("this build counts");
    let writes = each.take_writes();
    let copies = since(before, &each).copied;
    println!(
        "COUNTS m9.take_writes copied={copies} writes={}",
        writes.len()
    );
    assert!(
        u64::from(copies) >= MESSAGES * 4 / 5,
        "the control copied {copies} of {MESSAGES}"
    );
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
    let home::Boot { mut model, .. } =
        restore_settled(&mut flash, archive.address).expect("restore");
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
    let (taken, effects) = home::take(&mut model, rest, &archive.address);
    assert_eq!(taken, 72);
    // The loop routes what the page left (1493); the rows it hands the
    // summary log are named without a message copied (1521b).
    let routed = home::route(&mut model, effects, &mut || [7; 16]);
    let counts = since(before, &model);
    println!("COUNTS m9.pulled_page {counts}");
    assert!(
        counts.sorts <= 1 && counts.rows_built <= window(),
        "a page of 72: {counts} (bar sorts 1, rows_built {})",
        window()
    );
    assert_eq!(counts.copied, 0, "a pulled page copied: {counts}");
    assert!(!routed.rows.is_empty(), "the page's conversations owe rows");
}

/// The fixture kept, then booted once: the boot that finds no summary
/// folds the ring and writes the summary log from it (1521).
fn summarised(archive: &Archive) -> MemFlash {
    let mut flash = kept(archive);
    let boot = restore_settled(&mut flash, archive.address).expect("first boot");
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
    let boot = restore_settled(&mut flash, archive.address).expect("resume");
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
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    assert!(matches!(boot.how, home::How::Resumed { .. }));

    // The long chat: friend 0, every other entry of the first 1700.
    let peer = lxmf_address(&archive.friends[0].public_key_bytes()).expect("address");
    let ours: Vec<u64> = (1..=MESSAGES).filter(|&seq| friend_of(seq) == 0).collect();
    let want = home::page_entries();
    let (entries, earlier) = boot
        .shelf
        .page(&mut flash, peer, want, &archive.address)
        .expect("page");
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
    let (older, _) = boot
        .shelf
        .page(&mut flash, peer, want, &archive.address)
        .expect("older");
    let older: Vec<u64> = older
        .iter()
        .filter(|e| e.kind != loquax::home::KIND_MARK)
        .map(|e| e.seq)
        .collect();
    assert_eq!(older, ours[ours.len() - 2 * want..ours.len() - want]);
}

/// 1521, 1563: a page kept whose summary never got written (the power
/// went between the two) leaves the ring past the summary's end; the
/// boot reads the page the summary missed and nothing else, shows it,
/// writes the summary at the ring's end, and the boot after resumes
/// outright. Before 1563 this boot read the ring whole.
#[test]
fn a_ring_past_its_summary_reads_its_tail_alone() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let boot = restore_settled(&mut flash, archive.address).expect("resume");
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
    let after = restore_settled(&mut flash, archive.address).expect("after the cut");
    assert!(
        matches!(after.how, home::How::Resumed { tail: 1, .. }),
        "{:?}",
        after.how
    );
    assert_eq!(after.decoded, 1, "the tail alone is decoded");
    assert_eq!(after.shelf.cursor(), MESSAGES + 1);
    assert_eq!(after.restored.entries, MESSAGES + 1);
    let peer = lxmf_address(&friend.public_key_bytes()).expect("address");
    let top = &after.model.conversations[after.model.order()[0]];
    assert_eq!(top.peer, peer, "the page the summary missed is shown");
    let again = restore_settled(&mut flash, archive.address).expect("again");
    assert!(
        matches!(again.how, home::How::Resumed { tail: 0, .. }),
        "{:?}",
        again.how
    );
    let top = &again.model.conversations[again.model.order()[0]];
    assert_eq!(top.peer, peer);
}

/// The entries a pull brings after the fixture: `pages` pages of 72,
/// some to new peers, every fifth one the board's own send.
fn more(archive: &Archive, newcomers: &[Identity], pages: usize) -> Vec<Entry> {
    (1..=(pages * 72) as u64)
        .map(|n| {
            let seq = MESSAGES + n;
            let friend = match n % 3 {
                0 => &newcomers[n as usize % newcomers.len()],
                _ => &archive.friends[friend_of(n)],
            };
            let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
            let at = 1_762_100_000.0 + n as f64;
            let (kind, packed) = match n % 5 {
                0 => (
                    KIND_SENT,
                    pack(&archive.me, theirs, &format!("Neu {n}"), at),
                ),
                _ => (
                    KIND_RECEIVED,
                    pack(friend, archive.address, &format!("Neu {n}"), at),
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

/// The summary log of `flash` erased: the boot that follows folds the
/// ring whole, the control a resumed boot is held against.
fn without_summary(flash: &MemFlash) -> MemFlash {
    let mut erased = flash.clone();
    let (ring, _) = loquax::home::board::rows::split(2048).expect("split");
    for sector in ring..2048 {
        erased.sectors[sector as usize].fill(0xFF);
    }
    erased
}

/// 1563, the M9's erase and pull of 2026-11-08: the board reset by its
/// watchdog four times during a pull of 4258 entries, and every boot
/// after a reset folded the whole ring (`why=ring-moved`, 12.6 s at 1714
/// entries, 55.2 s at 4066), so each rebuild grew with the store. The
/// summary written before the reset stands for everything before it: the
/// boot reads the 360 entries the pull kept after it and not the 4000
/// before, lists what the whole fold of the same ring lists, and writes
/// the summary at the ring's end. On the host the tail boot is timed
/// against the whole fold; on the board the bar is under 2 s.
#[test]
fn a_reset_mid_pull_resumes_from_the_summary() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    let newcomers: Vec<Identity> = (0..5).map(|_| Identity::generate(&mut OsRng)).collect();
    let pulled = more(&archive, &newcomers, 5);
    for page in pulled.chunks(72) {
        home::keep(&mut boot.shelf, &mut flash, page, &archive.address).expect("keep");
    }
    // The reset: no rows said, no state written.
    let whole = home::restore(&mut without_summary(&flash), archive.address).expect("whole");
    assert!(matches!(whole.how, home::How::Rebuilt { why: "no-state" }));
    assert_eq!(whole.decoded, MESSAGES + pulled.len() as u64);

    let started = std::time::Instant::now();
    let mut after = home::restore_timed(&mut flash, archive.address, &micros).expect("after");
    let took = started.elapsed();
    println!(
        "TIME m9.tail-resume {took:?} decoded={} total_us={} (whole fold decoded {})",
        after.decoded, after.spent.total, whole.decoded
    );
    assert!(
        matches!(after.how, home::How::Resumed { tail: 360, .. }),
        "{:?}",
        after.how
    );
    assert_eq!(after.decoded, pulled.len() as u64, "the tail alone");
    assert_eq!(after.restored.entries, MESSAGES + pulled.len() as u64);
    assert_eq!(after.shelf.cursor(), MESSAGES + pulled.len() as u64);
    assert_eq!(
        after.model.conversations.len(),
        whole.model.conversations.len(),
        "the newcomers are listed"
    );
    assert_eq!(
        text(&after.model),
        text(&whole.model),
        "the list from the summary and its tail is the whole fold's"
    );
    for newcomer in &newcomers {
        let peer = lxmf_address(&newcomer.public_key_bytes()).expect("address");
        assert!(after.model.conversations.iter().any(|c| c.peer == peer));
    }
    // The summary went on to the ring's end, the tail boot's one bounded
    // write and the quiet turns after it (1597): the next boot reads
    // nothing.
    settle(&mut after.shelf, &mut flash, Vec::new());
    let again = restore_settled(&mut flash, archive.address).expect("again");
    assert!(
        matches!(again.how, home::How::Resumed { tail: 0, .. }),
        "{:?}",
        again.how
    );
    assert_eq!(again.decoded, 0);
    assert_eq!(text(&again.model), text(&whole.model));
}

/// 1563, the asks across a reset: an ask the summary kept and whose done
/// came after it is not queued again; an ask kept after the summary is.
#[test]
fn a_tail_resume_keeps_the_asks_its_tail_owes() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    let (old, new) = ([1u8; 16], [2u8; 16]);
    boot.shelf
        .keep_ask(&mut flash, &old, 3, b"vor dem Reset")
        .expect("keep the old ask");
    let owed = vec![loquax::home::board::store::PendingAsk {
        id: old,
        tag: 3,
        request: b"vor dem Reset".to_vec(),
    }];
    settle(&mut boot.shelf, &mut flash, owed);
    let newcomers = [Identity::generate(&mut OsRng)];
    for page in more(&archive, &newcomers, 1).chunks(72) {
        home::keep(&mut boot.shelf, &mut flash, page, &archive.address).expect("keep");
    }
    boot.shelf
        .keep_ask(&mut flash, &new, 4, b"nach dem Summary")
        .expect("keep the new ask");
    boot.shelf.ask_done(&mut flash, &old).expect("done");
    let after = restore_settled(&mut flash, archive.address).expect("after");
    assert!(
        matches!(after.how, home::How::Resumed { tail: 72, .. }),
        "{:?}",
        after.how
    );
    let ids: Vec<[u8; 16]> = after.asks.iter().map(|ask| ask.id).collect();
    assert_eq!(
        ids,
        vec![new],
        "the done ended the old ask, the new is owed"
    );
    let whole = home::restore(&mut without_summary(&flash), archive.address).expect("whole");
    let whole_ids: Vec<[u8; 16]> = whole.asks.iter().map(|ask| ask.id).collect();
    assert_eq!(ids, whole_ids, "as the whole fold has them");
}

/// 1563, the unhappy path: the sector the summary ended in is gone (the
/// ring went round past it): no tail can be read after it, and the boot
/// folds the ring whole, as before.
#[test]
fn a_summary_whose_sector_went_round_rebuilds() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    let (end_seq, _) = boot.shelf.ring.end();
    let newcomers = [Identity::generate(&mut OsRng)];
    for page in more(&archive, &newcomers, 3).chunks(72) {
        home::keep(&mut boot.shelf, &mut flash, page, &archive.address).expect("keep");
    }
    // The summary's sector, erased as the ring erases its oldest.
    let (ring, _) = loquax::home::board::rows::split(2048).expect("split");
    let sector = (0..ring as usize)
        .find(|&i| {
            let head = &flash.sectors[i];
            &head[..4] == b"LQXM"
                && u32::from_le_bytes([head[8], head[9], head[10], head[11]]) == end_seq
        })
        .expect("the summary's sector");
    flash.sectors[sector].fill(0xFF);
    let after = restore_settled(&mut flash, archive.address).expect("after");
    assert_eq!(after.how, home::How::Rebuilt { why: "ring-moved" });
    let again = restore_settled(&mut flash, archive.address).expect("again");
    assert!(
        matches!(again.how, home::How::Resumed { tail: 0, .. }),
        "{:?}",
        again.how
    );
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
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
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
    settle(&mut boot.shelf, &mut flash, Vec::new());

    let again = restore_settled(&mut flash, archive.address).expect("reboot");
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
    settle(&mut again.shelf, &mut flash, Vec::new());
    let third = restore_settled(&mut flash, archive.address).expect("third");
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

/// The fixture's archive and two conversations the fold makes that hold
/// no message (1521c): a group a friend invited the reader to, which no
/// message reached, and a chat whose only message a mark deleted. What
/// they add to the archive, and the two conversations' peers.
fn with_rowless(archive: &Archive) -> (Vec<Entry>, [[u8; 16]; 2]) {
    use leviculum_lxmf::lev::{Invite, Lev};
    use loquax::home::wire::{mark_request, Mark, MarkFlag};
    use loquax::home::KIND_MARK;
    let mut entries = archive.entries();
    let mut seq = MESSAGES;
    let mut at = |entries: &mut Vec<Entry>, kind: u8, packed: Vec<u8>| {
        seq += 1;
        entries.push(Entry {
            seq,
            kind,
            at: 1_762_000_000 + seq as i64,
            packed,
        });
    };
    let inviter = &archive.friends[3];
    let group = Identity::generate(&mut OsRng);
    let group_address = lxmf_address(&group.public_key_bytes()).expect("address");
    let mut fields = Vec::new();
    Lev {
        invite: Some(Invite {
            group: group_address,
            public_key: group.public_key_bytes(),
            name: "Chor".into(),
            admin_name: "Jürgen".into(),
            epoch: 1,
        }),
        ..Lev::default()
    }
    .put_into(&mut fields);
    let invitation = Message::create(
        archive.address,
        lxmf_address(&inviter.public_key_bytes()).expect("address"),
        inviter,
        1_762_000_000.0 + MESSAGES as f64 + 1.0,
        Vec::new(),
        b"Jurgen invited you to Chor".to_vec(),
        fields,
        DeliveryMethod::Direct,
    )
    .expect("create");
    at(&mut entries, KIND_RECEIVED, invitation.pack());
    let stranger = Identity::generate(&mut OsRng);
    let stranger_address = lxmf_address(&stranger.public_key_bytes()).expect("address");
    let gone = Message::create(
        archive.address,
        stranger_address,
        &stranger,
        1_762_000_000.0 + MESSAGES as f64 + 2.0,
        Vec::new(),
        b"deleted on another device".to_vec(),
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("create");
    let id = gone.message_id;
    at(&mut entries, KIND_RECEIVED, gone.pack());
    at(
        &mut entries,
        KIND_MARK,
        mark_request(&Mark {
            flag: MarkFlag::Deleted,
            ids: vec![id],
        }),
    );
    (entries, [group_address, stranger_address])
}

/// 1521c, the M9's bench: the boot that folded the ring said
/// `conversations=16`, every boot that resumed from the summary log after
/// it `conversations=15`. The fold made a conversation that holds no
/// message, for which `UiModel::row` has no row, so the summary log never
/// carried it. The two boots of one store list the same conversations:
/// the fold leaves out what the summaries cannot carry. The positive
/// control is the same entries folded page by page into a fresh model,
/// as the pull folds them: it holds both message-less conversations.
/// 1556: the invited group has the invitation's row ("Einladung") and is
/// listed on both boots; the chat whose message was deleted is on none.
#[test]
fn a_rebuilt_and_a_resumed_boot_count_the_same_conversations() {
    let archive = Archive::new();
    let (entries, rowless) = with_rowless(&archive);
    let mut flash = MemFlash::new(2048);
    let home::Boot {
        shelf: mut store, ..
    } = restore_settled(&mut flash, archive.address).expect("boot");
    for page in entries.chunks(72) {
        home::keep(&mut store, &mut flash, page, &archive.address).expect("keep");
    }

    let mut control = home::model(archive.address);
    for page in entries.chunks(72) {
        home::take(&mut control, page, &archive.address);
    }
    let [group, deleted] = rowless;
    for peer in rowless {
        let held = control.conversations.iter().find(|c| c.peer == peer);
        assert!(
            held.is_some_and(|c| c.messages.is_empty()),
            "the fold holds {peer:02x?} without a message"
        );
    }
    // 1556: the invitation's group has its row, the deleted chat none.
    let invitation = loquax::ui::strings::StrId::GroupInvitation.text(control.lang);
    assert_eq!(
        control.row(&group).map(|row| row.listing.line),
        Some(invitation.to_string()),
        "the invited group's row is the invitation's"
    );
    assert!(control.row(&deleted).is_none());
    assert_eq!(control.conversations.len(), CONVERSATIONS + 2);

    let rebuilt = restore_settled(&mut flash, archive.address).expect("rebuild");
    assert!(
        matches!(rebuilt.how, home::How::Rebuilt { .. }),
        "{:?}",
        rebuilt.how
    );
    let resumed = restore_settled(&mut flash, archive.address).expect("resume");
    assert!(
        matches!(resumed.how, home::How::Resumed { .. }),
        "{:?}",
        resumed.how
    );
    println!(
        "COUNT rebuilt={} resumed={} control={}",
        rebuilt.model.conversations.len(),
        resumed.model.conversations.len(),
        control.conversations.len()
    );
    assert_eq!(
        rebuilt.model.conversations.len(),
        resumed.model.conversations.len(),
        "the boot that folds the ring and the one that resumes count alike"
    );
    // The fixture's conversations and the invitation, on both boots.
    assert_eq!(rebuilt.model.conversations.len(), CONVERSATIONS + 1);
    for boot in [&rebuilt, &resumed] {
        assert!(boot.model.conversations.iter().any(|c| c.peer == group));
        assert!(boot.model.conversations.iter().all(|c| c.peer != deleted));
        assert_eq!(
            boot.model.row(&group).map(|row| row.listing.line),
            Some(invitation.to_string()),
            "listed as the invitation on every boot"
        );
    }
    let peers = |boot: &home::Boot| {
        let mut peers: Vec<[u8; 16]> = boot.model.conversations.iter().map(|c| c.peer).collect();
        peers.sort();
        peers
    };
    assert_eq!(peers(&rebuilt), peers(&resumed));
    assert_eq!(
        text(&rebuilt.model),
        text(&resumed.model),
        "and list the same rows"
    );
}

/// The microseconds since the first call, the clock a boot's breakdown
/// runs on (the board's `uptime_us`).
fn micros() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

/// 1521c, the M9's bench: the breakdown line of a boot outgrew the
/// board's log line (`LINE_MAX`, 160 bytes with its prefix and its `t=`)
/// and lost `rows_us` and `per_1000_ms`, and the phases it printed summed
/// to 49.5 s of a 100.2 s rebuild. Two lines now, each inside the log
/// line, every phase in them, the total and what no phase names; no
/// phase is counted twice, so the phases never sum past the total.
#[test]
fn a_boots_breakdown_says_every_phase_in_two_lines() {
    let archive = Archive::new();
    let mut flash = kept(&archive);
    let mut rebuilt = home::restore_timed(&mut flash, archive.address, &micros).expect("rebuild");
    settle(&mut rebuilt.shelf, &mut flash, Vec::new());
    let resumed = home::restore_timed(&mut flash, archive.address, &micros).expect("resume");
    assert!(matches!(rebuilt.how, home::How::Rebuilt { .. }));
    assert!(matches!(resumed.how, home::How::Resumed { .. }));
    for boot in [&rebuilt, &resumed] {
        let spent = boot.spent;
        assert!(
            spent.named() <= spent.total,
            "the phases sum past the total, one is counted twice: {spent:?}"
        );
        let lines = spent.lines(boot.how, boot.decoded);
        for line in &lines {
            println!("[STORE] {line}");
            // The board's line: prefix, body, " t=<u32>" and "\r\n".
            let board = "[STORE] ".len() + line.len() + " t=4294967295\r\n".len();
            assert!(board <= 160, "{board} bytes: {line}");
        }
        for key in [
            "decoded=",
            "read_us=",
            "crc_us=",
            "decode_us=",
            "unpack_us=",
            "step_us=",
            "finish_us=",
        ] {
            assert!(lines[0].contains(key), "{key} in {}", lines[0]);
        }
        for key in [
            "drain_us=",
            "rows_us=",
            "total_us=",
            "unnamed_us=",
            "per_1000_ms read=",
            " step=",
        ] {
            assert!(lines[1].contains(key), "{key} in {}", lines[1]);
        }
    }
    assert_eq!(rebuilt.decoded, MESSAGES);
    assert!(rebuilt.spent.drain > 0 && rebuilt.spent.rows > 0);
}

/// The archive with a mark after it per message of the long chat's
/// partner (friend 1): `n` read marks of one id each, as a board that
/// opened that chat again and again leaves them (the rig's ring at
/// 1556: 1730 marks of 4657 entries), then a deleted mark naming
/// `deleted`. Sequences go on after the messages.
fn with_marks(archive: &Archive, n: usize, deleted: [u8; 32]) -> Vec<Entry> {
    use loquax::home::wire::{mark_request, Mark, MarkFlag};
    let mut entries = archive.entries();
    let ids: Vec<[u8; 32]> = entries
        .iter()
        .filter(|e| friend_of(e.seq) == 1)
        .filter_map(|e| board::rows::entry_id(e, &archive.address))
        .collect();
    let mut seq = MESSAGES;
    let mut mark = |flag: MarkFlag, id: [u8; 32]| {
        seq += 1;
        Entry {
            seq,
            kind: loquax::home::KIND_MARK,
            at: 0,
            packed: mark_request(&Mark {
                flag,
                ids: vec![id],
            }),
        }
    };
    let marks: Vec<Entry> = ids
        .iter()
        .cycle()
        .take(n)
        .map(|&id| mark(MarkFlag::Read, id))
        .collect();
    entries.extend(marks);
    entries.push(mark(MarkFlag::Deleted, deleted));
    entries
}

/// 1556: a chat opens with its own marks, not the ring's. At 1521 a
/// page carried every mark of the ring from its oldest sector on: on the
/// rig a chat of under 30 messages opened with 1730 marks, read from 284
/// sectors (page_read 228 ms) and folded on the UI core (decode 858 ms
/// median, 1521c). Here a small chat, spread over the whole ring, opens
/// after 1800 marks of another chat: its page holds its own messages and
/// the one mark that names one of them, its deleted message stays
/// deleted after a resumed boot, and a mark the board sets after the
/// open files under the chat too. The host times the decode of the old
/// page against the new one.
#[test]
fn a_chat_opens_with_its_own_marks_only() {
    let archive = Archive::new();
    // Friend 30: a few messages from entry 2620 on, the ring's newest
    // sectors between them.
    let peer = lxmf_address(&archive.friends[30].public_key_bytes()).expect("address");
    let ours: Vec<Entry> = archive
        .entries()
        .into_iter()
        .filter(|e| friend_of(e.seq) == 30)
        .collect();
    assert!(ours.len() >= 3, "the fixture's small chat: {}", ours.len());
    let deleted = board::rows::entry_id(&ours[1], &archive.address).expect("id");
    let entries = with_marks(&archive, 1800, deleted);
    let mut flash = MemFlash::new(2048);
    let home::Boot {
        shelf: mut store, ..
    } = restore_settled(&mut flash, archive.address).expect("boot");
    for page in entries.chunks(72) {
        home::keep(&mut store, &mut flash, page, &archive.address).expect("keep");
    }
    store.dirty = true;
    settle(&mut store, &mut flash, Vec::new());
    assert_eq!(store.index.marks, Vec::<u32>::new(), "every mark placed");

    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    assert!(
        matches!(boot.how, home::How::Resumed { .. }),
        "{:?}",
        boot.how
    );
    let want = home::page_entries();
    let (page, earlier) = boot
        .shelf
        .page(&mut flash, peer, want, &archive.address)
        .expect("page");
    let cost = boot.shelf.index.last_page;
    println!("PAGE m9.small_chat {cost:?} earlier={earlier}");
    assert_eq!(earlier, 0);
    assert_eq!(cost.messages, ours.len());
    assert_eq!(cost.marks, 1, "the one mark that names the chat");
    assert_eq!(page.len(), ours.len() + 1);

    // The old page: the same messages and every mark of the ring from
    // the page's oldest sector on.
    let old: Vec<Entry> = ours
        .iter()
        .cloned()
        .chain(
            entries
                .iter()
                .filter(|e| e.kind == loquax::home::KIND_MARK)
                .cloned(),
        )
        .collect();
    let time = |page: &[Entry]| {
        let t0 = std::time::Instant::now();
        let messages =
            board::rows::messages::<loquax_m9::field::Field>(page, &archive.address, peer, None);
        (messages, t0.elapsed().as_micros())
    };
    let (before, before_us) = time(&old);
    let (after, after_us) = time(&page);
    println!(
        "DECODE m9.small_chat old_entries={} old_us={before_us} new_entries={} new_us={after_us}",
        old.len(),
        page.len()
    );
    let bodies =
        |m: &[loquax::ui::Message]| -> Vec<String> { m.iter().map(|m| m.body.clone()).collect() };
    assert_eq!(bodies(&after), bodies(&before), "the same bubbles");
    assert_eq!(after.len(), ours.len() - 1, "the deleted one stays deleted");
    assert!(
        after_us * 10 < before_us,
        "the page's decode ({after_us} µs) is a tenth of the old one's ({before_us} µs) or less"
    );

    // A mark the board sets after the open files under the chat: the
    // page taught the index its ids.
    let read = board::rows::entry_id(&ours[0], &archive.address).expect("id");
    let sectors_before = boot.shelf.index.kept[&peer].sectors.len();
    home::keep_marks(
        &mut boot.shelf,
        &mut flash,
        &[loquax::home::wire::Mark {
            flag: loquax::home::wire::MarkFlag::Read,
            ids: vec![read],
        }],
    )
    .expect("mark");
    assert!(
        boot.shelf.index.marks.is_empty(),
        "the board's mark is placed"
    );
    assert!(boot.shelf.index.kept[&peer].sectors.len() >= sectors_before);
}

/// One press of the board's key `code` as `ui_main` takes it (the
/// role's command, the key's event through the group table), and every
/// page the press asked read from the shelf and folded in as the board
/// folds it (`home::page_in` behind the list), until nothing is asked.
fn press_and_preload(
    boot: &mut home::Boot,
    flash: &mut MemFlash,
    address: &[u8; 16],
    row: &mut usize,
    code: u8,
) -> usize {
    use loquax_m9::keyboard::Key;
    let key = Key::of(code).expect("a key");
    let mut effects = Vec::new();
    if let Some(command) = key.command() {
        effects.extend(loquax_m9::keyboard::run(&mut boot.model, row, command));
    }
    if let Some(event) = key.event() {
        effects.extend(loquax_m9::group::press(&mut boot.model, row, &event));
    }
    let mut ids = || [0u8; 16];
    let mut pages = 0;
    loop {
        let routed = home::route(&mut boot.model, effects, &mut ids);
        if routed.loads.is_empty() {
            return pages;
        }
        effects = Vec::new();
        for peer in routed.loads {
            let (entries, earlier) = boot
                .shelf
                .page(flash, peer, home::page_entries(), address)
                .expect("page");
            effects.extend(home::page_in(
                &mut boot.model,
                peer,
                &entries,
                earlier,
                address,
            ));
            pages += 1;
        }
    }
}

/// The selected conversation's place in the list's order: what the
/// board's `[PAGE]` line says as `cursor=`.
fn cursor(model: &loquax::ui::UiModel<loquax_m9::field::Field>) -> usize {
    model
        .order()
        .iter()
        .position(|&i| i == model.selected)
        .expect("the selection is listed")
}

/// 1591, window 18: on a board booted from its summaries every row the
/// ring's cursor reaches preloads its page (1546), and the page's fold
/// gave its messages no time (the scratch fold knew none), so the row
/// sank to the list's end under the cursor: 0.6 rows a press, the walker
/// never reached the top from row 300. Twenty presses down move the
/// cursor exactly twenty rows, each a page read, and the list's order is
/// the one the summaries gave: on a boot that knows the time (the
/// preloaded messages carry their writers' times) and on one that does
/// not yet (the kept rows hold the order).
#[test]
fn twenty_presses_down_move_the_cursor_twenty_rows() {
    let archive = Archive::new();
    let flash = summarised(&archive);
    let clock = loquax::when::WallTime {
        unix_secs: 1_800_000_000,
        offset_secs: 0,
    };
    for now in [Some(clock), None] {
        walk_twenty(&archive, flash.clone(), now);
    }
    preloaded_rows_keep_their_places(&archive);
}

/// 1594, window 19: the arm "a preloaded row keeps its place among rows
/// with newer and older activity". The rows were kept by a pull whose
/// clock stood before the writers' (their clocks an hour ahead of the
/// home's), so every message's time was held to the pull's; an arrival
/// came after the pull and led the list; the reader then moved down it,
/// and every row the cursor reached preloaded its page under a later
/// clock, its messages came out with their writers' later times, and
/// the row rose over the arrival to the top (the bench's "Cem 2" over
/// "Bench", `open longest` four chats instead of fourteen). Each press
/// moves the cursor one row and no row moves.
fn preloaded_rows_keep_their_places(archive: &Archive) {
    const HOUR: i64 = 3600;
    let recorded_last = 1_762_000_000 + MESSAGES as i64;
    let pulled = loquax::when::WallTime {
        unix_secs: recorded_last + 10,
        offset_secs: 0,
    };
    // The pull: page by page into the ring and the model, the rows the
    // model said into the summary log, as the board's loop does.
    let mut flash = MemFlash::new(2048);
    let mut boot = restore_settled(&mut flash, archive.address).expect("empty boot");
    boot.model.now = Some(pulled);
    let mut ids = || [0u8; 16];
    let mut take = |boot: &mut home::Boot, flash: &mut MemFlash, page: &[Entry]| {
        home::keep(&mut boot.shelf, flash, page, &archive.address).expect("keep");
        let (_, effects) = home::take(&mut boot.model, page, &archive.address);
        let routed = home::route(&mut boot.model, effects, &mut ids);
        boot.shelf.said(routed.rows, routed.gone);
        settle(&mut boot.shelf, flash, Vec::new());
    };
    for page in archive.entries_ahead(HOUR as f64).chunks(72) {
        take(&mut boot, &mut flash, page);
    }
    // The boot after: listed from the rows, the clock two hours on, and
    // a message from a newcomer that arrived after the pull.
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    assert!(
        matches!(boot.how, home::How::Resumed { .. }),
        "{:?}",
        boot.how
    );
    boot.model.now = Some(loquax::when::WallTime {
        unix_secs: pulled.unix_secs + 2 * HOUR,
        ..pulled
    });
    let newcomer = Identity::generate(&mut OsRng);
    let arrived = pulled.unix_secs + 60;
    let arrival = Entry {
        seq: MESSAGES + 1,
        kind: KIND_RECEIVED,
        at: arrived,
        packed: pack(&newcomer, archive.address, "Neu hier", arrived as f64),
    };
    take(&mut boot, &mut flash, &[arrival]);
    let peers = |model: &loquax::ui::UiModel<loquax_m9::field::Field>| -> Vec<[u8; 16]> {
        model
            .order()
            .into_iter()
            .map(|i| model.conversations[i].peer)
            .collect()
    };
    let before = peers(&boot.model);
    assert_eq!(
        before[0],
        lxmf_address(&newcomer.public_key_bytes()).expect("address"),
        "the arrival after the pull leads the list"
    );
    let start = cursor(&boot.model);
    let mut row = start;
    let mut read = 0;
    for press in 1..=6 {
        read += press_and_preload(&mut boot, &mut flash, &archive.address, &mut row, 0xb6);
        assert_eq!(
            peers(&boot.model),
            before,
            "press {press}: a preloaded page moved a row"
        );
        assert_eq!(cursor(&boot.model), start + press, "press {press}");
    }
    assert!(read >= 6, "each row reached preloads its page: {read}");
}

/// 1595, window 20 (`bench/board.log` 857 to 865): the ring down put
/// the cursor on row 1, whose page preloaded (9 entries: 5 messages, 4
/// marks) and folded with no key, and 220 ms later the same row stood at
/// the top with the cursor on it; `cursor_to(1)` gave up and `open
/// longest` opened one chat of four. The page of a chat its reader read
/// on another device carries read marks recorded after its messages. A
/// pull of messages and then their marks, a resumed boot, the cursor
/// moved down the list row by row, each row's page with its marks folded
/// as the board folds it: no row moves, the cursor moves one row a press.
#[test]
fn a_preloaded_page_with_marks_keeps_its_row() {
    use loquax::home::wire::{mark_request, Mark, MarkFlag};
    let archive = Archive::new();
    let messages = archive.entries();
    let recorded_last = 1_762_000_000 + MESSAGES as i64;
    // Each friend's chat read on another device a while after the pull's
    // last message: a read mark per received message of the chat's
    // newest four, recorded then.
    let mut marks = Vec::new();
    let mut seq = MESSAGES;
    for friend in 0..CONVERSATIONS {
        let ids: Vec<[u8; 32]> = messages
            .iter()
            .filter(|e| friend_of(e.seq) == friend && e.kind == KIND_RECEIVED)
            .filter_map(|e| board::rows::entry_id(e, &archive.address))
            .collect();
        for id in ids.iter().rev().take(4) {
            seq += 1;
            marks.push(Entry {
                seq,
                kind: loquax::home::KIND_MARK,
                at: recorded_last + 600 + seq as i64,
                packed: mark_request(&Mark {
                    flag: MarkFlag::Read,
                    ids: vec![*id],
                }),
            });
        }
    }
    // The board's clock during the pull two minutes behind the home's,
    // which recorded the newest messages and the marks: the board takes
    // its day from the home once and counts on from its own uptime.
    let pulled = loquax::when::WallTime {
        unix_secs: recorded_last - 120,
        offset_secs: 0,
    };
    let mut flash = MemFlash::new(2048);
    let mut boot = restore_settled(&mut flash, archive.address).expect("empty boot");
    boot.model.now = Some(pulled);
    let mut ids = || [0u8; 16];
    let all: Vec<Entry> = messages.into_iter().chain(marks).collect();
    for page in all.chunks(72) {
        home::keep(&mut boot.shelf, &mut flash, page, &archive.address).expect("keep");
        let (_, effects) = home::take(&mut boot.model, page, &archive.address);
        let routed = home::route(&mut boot.model, effects, &mut ids);
        boot.shelf.said(routed.rows, routed.gone);
        settle(&mut boot.shelf, &mut flash, Vec::new());
    }
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    assert!(
        matches!(boot.how, home::How::Resumed { .. }),
        "{:?}",
        boot.how
    );
    boot.model.now = Some(loquax::when::WallTime {
        unix_secs: pulled.unix_secs + 7200,
        ..pulled
    });
    let peers = |model: &loquax::ui::UiModel<loquax_m9::field::Field>| -> Vec<[u8; 16]> {
        model
            .order()
            .into_iter()
            .map(|i| model.conversations[i].peer)
            .collect()
    };
    // The bench's arrival, as window 20's walk made it before the boot:
    // "Bench" a second over the newest chat, at the list's top.
    let arrival = loquax_m9::bench::arrival(&boot.model);
    let cursor_seq = boot.shelf.cursor();
    let bench = loquax_m9::bench::entries(archive.address, arrival, 1, cursor_seq);
    home::keep(&mut boot.shelf, &mut flash, &bench, &archive.address).expect("keep");
    let (_, effects) = home::take(&mut boot.model, &bench, &archive.address);
    let routed = home::route(&mut boot.model, effects, &mut ids);
    boot.shelf.said(routed.rows, routed.gone);
    settle(&mut boot.shelf, &mut flash, Vec::new());
    let mut boot = home::restore_shown(
        &mut flash,
        archive.address,
        &|| 0,
        loquax_m9::bench::Arrivals::Shown,
    )
    .expect("resume again");
    boot.model.now = Some(loquax::when::WallTime {
        unix_secs: pulled.unix_secs + 7200,
        ..pulled
    });
    assert_eq!(
        boot.model.conversations[boot.model.order()[0]].peer,
        loquax_m9::bench::BENCH_PEER,
        "Bench leads the list"
    );
    // Window 20's keys: Enter opens Bench (row 0), Back, the ring down.
    let mut row = 0;
    boot.model.selected = boot.model.order()[0];
    press_and_preload(&mut boot, &mut flash, &archive.address, &mut row, 0x0d);
    press_and_preload(&mut boot, &mut flash, &archive.address, &mut row, 0x86);
    assert_eq!(cursor(&boot.model), 0, "back on Bench");
    let before = peers(&boot.model);
    let start = cursor(&boot.model);
    let mut read = 0;
    for press in 1..=12 {
        read += press_and_preload(&mut boot, &mut flash, &archive.address, &mut row, 0xb6);
        let now = peers(&boot.model);
        if now != before {
            let moved: Vec<(usize, usize)> = before
                .iter()
                .enumerate()
                .filter_map(|(at, p)| {
                    let to = now.iter().position(|q| q == p)?;
                    (to != at).then_some((at, to))
                })
                .take(4)
                .collect();
            panic!("press {press}: a preloaded page with marks moved rows (from, to): {moved:?}");
        }
        assert_eq!(cursor(&boot.model), start + press, "press {press}");
    }
    assert!(read >= 12, "each row reached preloads its page: {read}");
}

fn walk_twenty(archive: &Archive, mut flash: MemFlash, now: Option<loquax::when::WallTime>) {
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");
    assert!(matches!(boot.how, home::How::Resumed { .. }));
    // The board's case: its rows were written by a pull that knew the
    // time (the home says it before it hands a page), so each row's
    // `moved` is its newest message's.
    boot.model.now = now;
    let mut newest_of = std::collections::BTreeMap::new();
    for conversation in &mut boot.model.conversations {
        let friend = archive
            .friends
            .iter()
            .position(|f| {
                lxmf_address(&f.public_key_bytes()).expect("address") == conversation.peer
            })
            .expect("a friend's");
        let newest = (1..=MESSAGES)
            .filter(|&seq| friend_of(seq) == friend)
            .max()
            .expect("a message");
        let moved = 1_762_000_000 + newest as i64;
        newest_of.insert(conversation.peer, moved);
        let listing = conversation.listing.as_mut().expect("listed from its row");
        listing.moved = Some(moved);
    }
    let peers = |model: &loquax::ui::UiModel<loquax_m9::field::Field>| -> Vec<[u8; 16]> {
        model
            .order()
            .into_iter()
            .map(|i| model.conversations[i].peer)
            .collect()
    };
    let before = peers(&boot.model);
    assert_eq!(before.len(), CONVERSATIONS);
    let start = cursor(&boot.model);
    let mut row = 0;
    let mut read = 0;
    for press in 1..=20 {
        read += press_and_preload(&mut boot, &mut flash, &archive.address, &mut row, 0xb6);
        assert_eq!(
            cursor(&boot.model),
            start + press,
            "press {press} of the ring down, the time known: {}",
            now.is_some()
        );
    }
    assert!(read >= 20, "each row reached preloads its page: {read}");
    assert_eq!(
        peers(&boot.model),
        before,
        "reading the pages moved no row in the list"
    );
    // A boot that knows the time gives the preloaded messages theirs.
    let selected = &boot.model.conversations[boot.model.selected];
    let newest = selected
        .messages
        .iter()
        .filter_map(|m| m.at)
        .map(|at| at.unix_secs)
        .max();
    // A boot that does not know it yet gives them the home's record of
    // them all the same (1597): the time is the entry's, not the clock's.
    assert_eq!(newest, newest_of.get(&selected.peer).copied());
}

/// 1591, window 18: a write of every row to the summary log (the bench's
/// `owe erase`, an eviction, a snapshot) was 644 records, 32 sector
/// erases, in one turn of the I/O core, the UI core parked for 2.8 s of
/// 5 and Enter unanswered. Bounded, it goes a few sectors a call; a boot
/// between two calls still lists every row from the records before
/// them, and the boot after the last lists the same. A snapshot begun
/// goes the same way, its SNAP record with its last records.
#[test]
fn a_write_of_every_row_goes_a_few_sectors_a_call() {
    let archive = Archive::new();
    let mut flash = summarised(&archive);
    let listed = |flash: &MemFlash| -> Vec<[u8; 16]> {
        let boot = home::restore(&mut flash.clone(), archive.address).expect("boot");
        assert!(
            matches!(boot.how, home::How::Resumed { .. }),
            "{:?}",
            boot.how
        );
        boot.model
            .order()
            .into_iter()
            .map(|i| boot.model.conversations[i].peer)
            .collect()
    };
    let before = listed(&flash);
    assert_eq!(before.len(), CONVERSATIONS);
    let mut boot = restore_settled(&mut flash, archive.address).expect("resume");

    // Every row owed, as the bench's owe fires it.
    boot.shelf.index.owe_all();
    boot.shelf.dirty = true;
    let mut calls = 0;
    let mut records = 0;
    while boot.shelf.index.owes() || boot.shelf.dirty {
        let done = boot.shelf.flush(&mut flash, Vec::new).expect("flush");
        let done = done.appended();
        assert!(
            done.erased <= ERASE_BUDGET,
            "call {calls} erased {} sectors",
            done.erased
        );
        records += done.entries;
        calls += 1;
        if calls == 1 {
            assert_eq!(listed(&flash), before, "a boot after the first call");
        }
        assert!(calls < 200, "the owed records never ran out");
    }
    assert!(calls > 4, "{records} records in {calls} calls: not split");
    assert_eq!(listed(&flash), before, "the boot after the last call");

    // A snapshot begun is written the same way.
    let log = boot.shelf.log.as_mut().expect("a summary log");
    log.begin_snapshot(&mut boot.shelf.index);
    let mut calls = 0;
    while boot.shelf.log.as_ref().expect("log").snapping() {
        let done = boot.shelf.flush(&mut flash, Vec::new).expect("flush");
        assert!(
            done.appended().erased <= ERASE_BUDGET,
            "snapshot call {calls} erased {}",
            done.appended().erased
        );
        calls += 1;
        assert!(calls < 200, "the snapshot never ended");
    }
    assert!(calls > 4, "the snapshot in {calls} calls: not split");
    assert_eq!(listed(&flash), before, "the boot after the snapshot");
}

/// A [`Sectors`] that counts the erases its caller made in `from..`, the
/// summary log's sectors.
struct Counting<'a> {
    flash: &'a mut MemFlash,
    from: u32,
    erases: u32,
}

impl Sectors for Counting<'_> {
    type Error = <MemFlash as Sectors>::Error;

    fn count(&self) -> u32 {
        self.flash.count()
    }

    fn read(&mut self, sector: u32, offset: usize, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(sector, offset, buf)
    }

    fn write(&mut self, sector: u32, offset: usize, data: &[u8]) -> Result<(), Self::Error> {
        self.flash.write(sector, offset, data)
    }

    fn erase(&mut self, sector: u32) -> Result<(), Self::Error> {
        if sector >= self.from {
            self.erases += 1;
        }
        self.flash.erase(sector)
    }
}

/// The board's partition: 1024 sectors, the summary log the last 128.
const BOARD_SECTORS: u32 = 1024;

/// The archive with every friend's name, as the board's store holds them
/// (window 21: 342 names and 303 rows in the summary log): a write of
/// every row owes the names first.
fn named_entries(archive: &Archive) -> Vec<Entry> {
    let mut entries = archive.entries();
    let mut seq = MESSAGES;
    for (n, friend) in archive.friends.iter().enumerate() {
        seq += 1;
        let peer = lxmf_address(&friend.public_key_bytes()).expect("address");
        entries.push(Entry {
            seq,
            kind: loquax::home::KIND_NAME,
            at: 1_762_000_000 + seq as i64,
            packed: loquax::home::wire::name_entry(&peer, &format!("Freund Nummer {n}")),
        });
    }
    entries
}

/// 1597, window 21's hang.walk: after 1595 the board still took 1 to
/// 1.6 s eight times, each right after a `[STORE] summary records=246..473
/// erased=22..31` line. The walk's bench owes every row every few seconds
/// (`[BENCH] owe=fired`), each owe re-owed the 342 names first, the
/// snapshot begun ended only when nothing at all was owed, never did, the
/// log came round to it late, and the writer dropped its budget and
/// wrote the 303 rows in one call. Replayed here through every caller of
/// the writer on the board's partition: the boot's rebuild, the bench's
/// owe (`Client::flush`), the quiet turns (`Shelf::quiet_flush`), an
/// arrival's rows, the boot that resumes a tail. No call opens more than
/// `ERASE_BUDGET` sectors of the log, the log comes round many times,
/// and the boot after it lists every conversation.
#[test]
fn every_summary_write_stays_in_its_budget_on_the_hang_walk() {
    let archive = Archive::new();
    let entries = named_entries(&archive);
    let mut flash = MemFlash::new(BOARD_SECTORS);
    let (ring_count, _) = loquax::home::board::rows::split(BOARD_SECTORS).expect("split");
    let mut worst: Vec<(String, u32)> = Vec::new();
    let mut note = |caller: &str, erased: u32| {
        if erased > ERASE_BUDGET {
            worst.push((caller.to_string(), erased));
        }
    };

    // The pull keeps the ring, no summary written yet (a join).
    {
        let home::Boot { mut shelf, .. } =
            home::restore(&mut flash, archive.address).expect("empty boot");
        for page in entries.chunks(72) {
            home::keep(&mut shelf, &mut flash, page, &archive.address).expect("keep");
        }
    }
    // The boot that finds no summary rebuilds and writes it whole before
    // it returns (1603): many bounded writes, each in the budget.
    let mut counting = Counting {
        flash: &mut flash,
        from: ring_count,
        erases: 0,
    };
    let mut boot = home::restore(&mut counting, archive.address).expect("rebuild");
    assert_eq!(
        counting.erases, boot.settled.erased,
        "the rebuild's summary writes are all it erased of the log"
    );
    assert!(!boot.shelf.owes(), "the rebuild left the summary owed");
    note("boot rebuild", boot.settle_most_erased);
    assert!(
        matches!(boot.how, home::How::Rebuilt { .. }),
        "{:?}",
        boot.how
    );

    let mut now_ms = 0u64;
    let mut seq = MESSAGES + archive.friends.len() as u64 + 1;
    let mut owes = 0;
    let used_before = boot.shelf.log.as_ref().expect("log").used();
    let mut log_sectors_written = 0u32;
    for cycle in 0..400i64 {
        // The bench's owe: every row and name owed, written at once.
        boot.shelf.index.owe_all();
        boot.shelf.dirty = true;
        owes += 1;
        let done = boot.shelf.flush(&mut flash, Vec::new).expect("owe");
        note("bench owe", done.appended().erased);
        log_sectors_written += done.appended().erased;
        // The walk's keys, a second apart: the quiet turns between.
        for _ in 0..2 {
            now_ms += 1_000;
            let done = boot
                .shelf
                .quiet_flush(&mut flash, Vec::new, now_ms)
                .expect("quiet");
            note("quiet flush", done.appended().erased);
            log_sectors_written += done.appended().erased;
            boot.shelf.quiet(&mut flash).expect("quiet erase");
        }
        // An arrival: kept in the ring, its rows said, written.
        let arrival = loquax_m9::bench::Arrival {
            at: 1_763_000_000 + cycle,
            named: cycle > 0,
        };
        let new = loquax_m9::bench::entries(archive.address, arrival, cycle as u32, seq);
        seq += new.len() as u64;
        home::keep(&mut boot.shelf, &mut flash, &new, &archive.address).expect("arrive");
        boot.shelf.said(Vec::new(), Vec::new());
        let done = boot.shelf.flush(&mut flash, Vec::new).expect("said");
        note("arrival rows", done.appended().erased);
        log_sectors_written += done.appended().erased;
    }
    assert!(
        log_sectors_written > 2 * 128,
        "the log came round {log_sectors_written} sectors from {used_before}: not the walk's"
    );
    assert!(owes == 400);

    // Drained in quiet turns, then a boot that resumes lists them all.
    for _ in 0..400 {
        now_ms += 1_000;
        let done = boot
            .shelf
            .quiet_flush(&mut flash, Vec::new, now_ms)
            .expect("drain");
        note("quiet flush", done.appended().erased);
        if !boot.shelf.owes() {
            break;
        }
    }
    assert!(!boot.shelf.owes(), "the quiet turns never drained the log");
    // A page kept with no summary after it: the next boot resumes the
    // tail and writes the summary, one bounded write.
    let tail = loquax_m9::bench::entries(
        archive.address,
        loquax_m9::bench::Arrival {
            at: 1_764_000_000,
            named: true,
        },
        9_999,
        seq,
    );
    home::keep(&mut boot.shelf, &mut flash, &tail, &archive.address).expect("tail");
    drop(boot);
    let mut counting = Counting {
        flash: &mut flash,
        from: ring_count,
        erases: 0,
    };
    let again = home::restore_shown(
        &mut counting,
        archive.address,
        &|| 0,
        loquax_m9::bench::Arrivals::Shown,
    )
    .expect("resume");
    note("boot resume tail", counting.erases);
    assert!(
        matches!(again.how, home::How::Resumed { tail, .. } if tail > 0),
        "{:?}",
        again.how
    );
    assert_eq!(
        again.model.conversations.len(),
        CONVERSATIONS + 1,
        "every conversation and Bench survive the log's rounds"
    );
    assert!(
        worst.is_empty(),
        "summary writes past {ERASE_BUDGET} erases: {worst:?}"
    );
}

/// What [`no_caller_can_write_the_summary_unbounded`] refuses: a summary
/// write that takes its budget from the caller, `None` for all of it.
fn unbounded_writes(source: &str) -> Vec<&'static str> {
    ["flush_within(", "write_within(", "budget: Option<usize>"]
        .into_iter()
        .filter(|pattern| source.contains(pattern))
        .collect()
}

/// 1597: the bound lives in the writer, so no caller can ask for more.
/// Before, `RowLog::write_within` and `Shelf::flush_within` took
/// `Option<usize>`, `None` wrote everything owed, and the boot's flushes
/// passed it. The writer's sources and every caller's are read: none
/// may take a budget again. The positive control is the API as it was.
#[test]
fn no_caller_can_write_the_summary_unbounded() {
    let control = "fn write_within(&mut self, budget: Option<usize>) {}";
    assert_eq!(
        unbounded_writes(control).len(),
        2,
        "the scan finds the old API"
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for file in [
        "loquax/src/home/board/rows.rs",
        "loquax-m9/src/home.rs",
        "loquax-m9/src/bin/loquax-m9-sim/board.rs",
        "leviculum-esp/src/handheld.rs",
    ] {
        let source = std::fs::read_to_string(root.join(file)).expect(file);
        assert!(
            source.contains("flush") || source.contains("write"),
            "{file} read"
        );
        assert_eq!(unbounded_writes(&source), Vec::<&str>::new(), "{file}");
    }
}

/// 1597, window 21's bench.walk: `[PAGE_MOVED] peer=f18f4927
/// last_written=Some(…816)->Some(…832) moved=Some(…816)`. The bench
/// sender's "mark control" reached the home before the marks walk; the
/// board preloaded the sender's page at boot and took its day from that
/// page's newest stamp (`Reader::saw`, the model told on the next
/// second's tick), then the pull brought the message, recorded 16 s past
/// that day, and folded it in the same turn, the model's clock not yet
/// told the page's stamp. Each home entry is folded under the clock held
/// to when the home recorded it (`recorded_now`), and that hold took the
/// earlier of the two: the message came out at the stale day, and the
/// row the summary kept said so. After a reboot the clock is past the
/// stamp, the page's fold gave the message its recorded time, and the
/// row rose. Replayed here as the board ran it: preload, the pull in
/// the same turn, the open and the home's read mark, the reboot, the
/// cursor on the sender, its page read and folded. The row's time does
/// not move.
#[test]
fn a_page_read_after_a_reboot_keeps_its_rows_time() {
    use loquax::home::wire::{mark_request, Mark, MarkFlag};
    use loquax_m9::read::Reader;
    let archive = Archive::new();
    let address = archive.address;
    let mut flash = summarised(&archive);
    let mut boot = restore_settled(&mut flash, address).expect("resume");
    let friend = &archive.friends[30];
    let peer = lxmf_address(&friend.public_key_bytes()).expect("address");
    let find = |model: &loquax::ui::UiModel<loquax_m9::field::Field>| {
        model
            .conversations
            .iter()
            .position(|c| c.peer == peer)
            .expect("the sender's conversation")
    };
    let mut ids = || [0u8; 16];

    // The boot preloads the sender's page; the next second tells the
    // model the day from its newest stamp.
    let mut reader = Reader::default();
    let (page, _) = boot
        .shelf
        .page(&mut flash, peer, home::page_entries(), &address)
        .expect("page");
    reader.saw(&page, 5_400);
    let _ = reader.tick(&mut boot.model, 6_000);
    let day = boot.model.now.expect("the day from the page").unix_secs;

    // The pull brings "mark control", written and recorded 16 s past
    // that day, and folds it in the same turn: seen, not yet told.
    let recorded = day + 16;
    let control = Entry {
        seq: boot.shelf.cursor() + 1,
        kind: KIND_RECEIVED,
        at: recorded,
        packed: pack(friend, address, "mark control", recorded as f64),
    };
    let id = board::rows::entry_id(&control, &address).expect("id");
    let mut pulled = |boot: &mut home::Boot, flash: &mut MemFlash, page: &[Entry]| {
        reader.saw(page, 6_027);
        home::keep(&mut boot.shelf, flash, page, &address).expect("keep");
        let (_, effects) = home::take(&mut boot.model, page, &address);
        let routed = home::route(&mut boot.model, effects, &mut ids);
        boot.shelf.said(routed.rows, routed.gone);
        settle(&mut boot.shelf, flash, Vec::new());
    };
    pulled(&mut boot, &mut flash, std::slice::from_ref(&control));
    // The walk opens it; the home's read mark comes back.
    let mark = Entry {
        seq: control.seq + 1,
        kind: loquax::home::KIND_MARK,
        at: recorded + 2,
        packed: mark_request(&Mark {
            flag: MarkFlag::Read,
            ids: vec![id],
        }),
    };
    pulled(&mut boot, &mut flash, &[mark]);

    // The reboot: the row from the summary, the clock past the stamp.
    let mut boot = restore_settled(&mut flash, address).expect("reboot");
    assert!(
        matches!(boot.how, home::How::Resumed { .. }),
        "{:?}",
        boot.how
    );
    boot.model.now = Some(loquax::when::WallTime {
        unix_secs: recorded + 60,
        offset_secs: 0,
    });
    let at = find(&boot.model);
    let listed = boot.model.conversations[at].last_written();
    let place = boot.model.order().iter().position(|&i| i == at);
    boot.model.selected = at;
    let (page, earlier) = boot
        .shelf
        .page(&mut flash, peer, home::page_entries(), &address)
        .expect("page");
    assert!(
        page.iter().any(|e| e.seq == control.seq),
        "the page holds the message"
    );
    home::page_in(&mut boot.model, peer, &page, earlier, &address);
    let at = find(&boot.model);
    assert_eq!(
        boot.model.conversations[at].last_written(),
        listed,
        "the page's fold moved the row's time"
    );
    assert_eq!(
        boot.model.order().iter().position(|&i| i == at),
        place,
        "the page's fold moved the row"
    );
    assert_eq!(listed, Some(recorded), "the row says when the home took it");
}
