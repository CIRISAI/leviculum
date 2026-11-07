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
    let (_, mut store, _, _) = home::restore(&mut flash, archive.address).expect("boot");
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

    let (model, _, restored, _) = home::restore(&mut flash, archive.address).expect("restore");
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
    each.selected = 0;
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
    let (mut model, _, _, _) = home::restore(&mut flash, archive.address).expect("restore");
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
