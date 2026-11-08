//! The bench never touches a real chat (1562): an arrival lands in one
//! synthetic conversation, "Bench", and nowhere else; a release image's
//! restore lists no "Bench", from a rebuilt ring and from a summary
//! alike; the board asks its home no mark for an arrival; and `bench
//! forget` takes the arrivals out of the ring, the flagged ones and the
//! unflagged ones order 1540 left in real chats, and nothing of the
//! reader's.

use leviculum_core::Identity;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::ask::Verb;
use loquax::home::board::store::MemFlash;
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED};
use loquax::ui::UiModel;
use loquax_m9::bench::{self, Arrivals, BENCH_NAME, BENCH_PEER};
use loquax_m9::field::Field;
use loquax_m9::group;
use loquax_m9::home::{self, Boot, How, Shelf};
use loquax_m9::keys::{event, M9Key};
use loquax_m9::screen::{self, Look, COLS, ROWS};
use rand_core::OsRng;

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

/// An arrival as order 1540 made it: into a real chat, from that chat's
/// peer, its signature zero, no flag.
fn unflagged(peer: [u8; 16], address: [u8; 16], at: i64, n: u32, seq: u64) -> Entry {
    let words = format!("bench arrival {n}");
    let mut packed = Vec::new();
    packed.extend_from_slice(&address);
    packed.extend_from_slice(&peer);
    packed.extend_from_slice(&[0u8; 64]);
    msgpack::array(&mut packed, 4);
    msgpack::f64(&mut packed, at as f64);
    msgpack::bin(&mut packed, b"");
    msgpack::bin(&mut packed, words.as_bytes());
    msgpack::map(&mut packed, 0);
    Entry {
        seq,
        kind: KIND_RECEIVED,
        at,
        packed,
    }
}

fn list_text(model: &UiModel<Field>) -> String {
    screen::lines(&screen::scene(model), &Look::of(model), COLS, ROWS)
        .iter()
        .map(|line| line.cells.iter().map(|c| c.ch).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The chats and their message bodies, "Bench" left out.
fn real_chats(model: &UiModel<Field>) -> Vec<([u8; 16], Vec<String>)> {
    let mut chats: Vec<_> = model
        .conversations
        .iter()
        .filter(|c| c.peer != BENCH_PEER)
        .map(|c| (c.peer, c.messages.iter().map(|m| m.body.clone()).collect()))
        .collect();
    chats.sort();
    chats
}

/// A board of a home with two friends' chats, pulled and summarised:
/// what a walk starts from.
struct Bench {
    address: [u8; 16],
    flash: MemFlash,
    model: UiModel<Field>,
    shelf: Shelf,
    friends: Vec<[u8; 16]>,
    arrivals: u32,
}

impl Bench {
    fn new() -> Self {
        let me = Identity::generate(&mut OsRng);
        let address = lxmf_address(&me.public_key_bytes()).expect("address");
        let friends: Vec<Identity> = (0..2).map(|_| Identity::generate(&mut OsRng)).collect();
        let page: Vec<Entry> = (0..6)
            .map(|i| Entry {
                seq: i + 1,
                kind: KIND_RECEIVED,
                at: 1_762_000_000 + i as i64,
                packed: pack(
                    &friends[i as usize % 2],
                    address,
                    &format!("hallo {i}"),
                    1_762_000_000.0 + i as f64,
                ),
            })
            .collect();
        // 64 sectors: the ring and a summary log beside it.
        let mut flash = MemFlash::new(64);
        let Boot {
            mut model, shelf, ..
        } = home::restore_shown(&mut flash, address, &|| 0, Arrivals::Shown).expect("boot");
        let mut bench = Self {
            address,
            flash,
            model: home::model(address),
            shelf,
            friends: friends
                .iter()
                .map(|f| lxmf_address(&f.public_key_bytes()).expect("address"))
                .collect(),
            arrivals: 0,
        };
        let (_, effects) = home::take(&mut model, &page, &address);
        home::keep(&mut bench.shelf, &mut bench.flash, &page, &address).expect("keep");
        bench.model = model;
        bench.settle(effects);
        bench
    }

    /// What the board's loop does after a fold: the rows said and the
    /// summary written.
    fn settle(&mut self, effects: Vec<loquax::ui::UiEffect>) -> home::Routed {
        let routed = home::route(&mut self.model, effects, &mut || [9; 16]);
        self.shelf.said(routed.rows.clone(), routed.gone.clone());
        home::keep_marks(&mut self.shelf, &mut self.flash, &routed.marks).expect("marks");
        self.shelf.flush(&mut self.flash, Vec::new).expect("flush");
        routed
    }

    /// `bench arrive`, as the two cores do it: the UI core's arrival,
    /// the I/O core's entries kept and handed back, the fold, the
    /// cursor on "Bench".
    fn arrive(&mut self) {
        self.arrivals += 1;
        let arrival = bench::arrival(&self.model);
        let entries = bench::entries(self.address, arrival, self.arrivals, self.shelf.cursor());
        home::keep(&mut self.shelf, &mut self.flash, &entries, &self.address).expect("keep");
        let (_, effects) = home::take(&mut self.model, &entries, &self.address);
        assert!(bench::select(&mut self.model), "Bench is there");
        self.settle(effects);
    }

    fn boot(&mut self, arrivals: Arrivals) -> Boot {
        home::restore_shown(&mut self.flash, self.address, &|| 0, arrivals).expect("boot")
    }
}

/// `bench arrive` three times: one conversation more, "Bench", unread
/// three and at the list's top under the cursor; every real chat holds
/// what it held; and the board asks its home for no mark when the walk
/// opens "Bench", but keeps the read on the board.
#[test]
fn an_arrival_lands_in_bench_and_nowhere_else() {
    let mut b = Bench::new();
    let before = real_chats(&b.model);
    assert_eq!(before.len(), 2);
    for _ in 0..3 {
        b.arrive();
    }
    assert_eq!(real_chats(&b.model), before, "no real chat moved");
    let at = bench::index(&b.model).expect("Bench");
    let chat = &b.model.conversations[at];
    assert_eq!(chat.unread, 3);
    assert_eq!(
        chat.messages
            .iter()
            .map(|m| m.body.as_str())
            .collect::<Vec<_>>(),
        ["bench arrival 1", "bench arrival 2", "bench arrival 3"]
    );
    assert_eq!(b.model.order()[0], at, "newest, at the top");
    assert_eq!(b.model.selected, at, "the cursor on it");
    assert!(
        list_text(&b.model).contains(BENCH_NAME),
        "{}",
        list_text(&b.model)
    );

    // The walk opens it: the board marks it read and keeps the mark,
    // and asks its home nothing.
    let mut row = 0;
    let effects = group::press(&mut b.model, &mut row, &event(M9Key::Enter));
    let routed = b.settle(effects);
    assert_eq!(routed.marks.len(), 1, "the read is kept on the board");
    assert!(
        !routed.verbs.iter().any(|v| matches!(v, Verb::Mark(_))),
        "no mark for an arrival goes to the home: {:?}",
        routed.verbs
    );
    // The control: a real chat's mark does go.
    b.model.selected = b
        .model
        .conversations
        .iter()
        .position(|c| c.peer == b.friends[0])
        .expect("friend");
    b.model.focus = loquax::ui::Pane::Conversations;
    let effects = group::press(&mut b.model, &mut row, &event(M9Key::Enter));
    let routed = home::route(&mut b.model, effects, &mut || [9; 16]);
    assert!(
        routed.verbs.iter().any(|v| matches!(v, Verb::Mark(_))),
        "the friend's read goes to the home: {:?}",
        routed.verbs
    );
}

/// A release image's boot lists no "Bench", whether it rebuilds from the
/// ring or resumes from the summary; the bench image's boot lists it
/// both ways. The real chats are the same in all four.
#[test]
fn a_release_boot_shows_no_bench() {
    let mut b = Bench::new();
    b.arrive();
    b.arrive();
    let real = real_chats(&b.model);
    // From the summary the bench image wrote.
    let shown = b.boot(Arrivals::Shown);
    assert!(matches!(shown.how, How::Resumed { .. }), "{:?}", shown.how);
    assert!(
        bench::index(&shown.model).is_some(),
        "the bench image lists it"
    );
    let release = b.boot(Arrivals::Dropped);
    assert!(
        matches!(release.how, How::Resumed { .. }),
        "{:?}",
        release.how
    );
    assert_eq!(bench::index(&release.model), None);
    assert!(!list_text(&release.model).contains(BENCH_NAME));
    assert_eq!(real_chats(&release.model).len(), real.len());

    // From the ring: the summary gone, both rebuild.
    let mut ring_only = MemFlash::new(64);
    for sector in 0..56 {
        let mut buf = vec![0u8; 4096];
        use loquax::home::board::store::Sectors;
        b.flash.read(sector, 0, &mut buf).expect("read");
        if buf.iter().any(|&x| x != 0xFF) {
            ring_only.write(sector, 0, &buf).expect("write");
        }
    }
    let release = home::restore_timed(&mut ring_only.clone(), b.address, &|| 0).expect("boot");
    assert!(
        matches!(release.how, How::Rebuilt { .. }),
        "{:?}",
        release.how
    );
    assert_eq!(
        bench::index(&release.model),
        None,
        "rebuilt, still no Bench"
    );
    assert_eq!(real_chats(&release.model), real);
    let shown =
        home::restore_shown(&mut ring_only, b.address, &|| 0, Arrivals::Shown).expect("boot");
    assert!(
        bench::index(&shown.model).is_some(),
        "the control: the ring has it"
    );
}

/// `bench forget` on a board that ran 1540's image and then this one:
/// 1540's unflagged arrivals in a real chat, the flagged ones in
/// "Bench", the read marks of both. After it and a boot: no arrival
/// anywhere, every real message and the reader's own read still there,
/// the summary erased so the boot rebuilt. A second forget finds
/// nothing and writes nothing.
#[test]
fn bench_forget_takes_every_arrival_out_and_nothing_else() {
    let mut b = Bench::new();
    let real = real_chats(&b.model);
    // 1540's: two arrivals into the first friend's chat.
    for n in 1..=2 {
        let entry = unflagged(
            b.friends[0],
            b.address,
            1_762_000_100 + n as i64,
            n,
            b.shelf.cursor(),
        );
        assert!(bench::is_unflagged(&entry) && !bench::is_bench(&entry));
        home::keep(
            &mut b.shelf,
            &mut b.flash,
            std::slice::from_ref(&entry),
            &b.address,
        )
        .expect("keep");
        let (_, effects) = home::take(&mut b.model, &[entry], &b.address);
        b.settle(effects);
    }
    for _ in 0..3 {
        b.arrive();
    }
    // The reader opens the friend's chat: one mark names the real
    // unread and the unflagged arrivals together, and stays.
    let friend = b
        .model
        .conversations
        .iter()
        .position(|c| c.peer == b.friends[0])
        .expect("friend");
    b.model.selected = friend;
    let mut row = 0;
    let effects = group::press(&mut b.model, &mut row, &event(M9Key::Enter));
    b.settle(effects);
    let _ = group::press(&mut b.model, &mut row, &event(M9Key::Back));
    // The walk opens Bench: a mark of nothing but arrivals.
    b.model.selected = bench::index(&b.model).expect("Bench");
    let effects = group::press(&mut b.model, &mut row, &event(M9Key::Enter));
    b.settle(effects);

    assert!(
        real_chats(&b.model)
            .iter()
            .any(|(peer, bodies)| *peer == b.friends[0]
                && bodies.iter().any(|m| m.starts_with("bench arrival"))),
        "the control: 1540's arrivals sit in the friend's chat"
    );

    let fed = std::cell::Cell::new(0u32);
    let forgot = home::forget(&mut b.flash, b.address, &|| fed.set(fed.get() + 1)).expect("forget");
    assert_eq!(forgot.unflagged, 2, "{forgot:?}");
    assert_eq!(
        forgot.flagged, 4,
        "three arrivals and Bench's name: {forgot:?}"
    );
    assert!(forgot.marks >= 1, "Bench's read mark: {forgot:?}");
    assert_eq!(
        forgot.summary_erased, 8,
        "the whole summary log: {forgot:?}"
    );
    assert!(fed.get() >= forgot.store.sectors + 8);

    let boot = b.boot(Arrivals::Shown);
    assert!(matches!(boot.how, How::Rebuilt { .. }), "{:?}", boot.how);
    assert_eq!(bench::index(&boot.model), None, "no Bench");
    assert_eq!(
        real_chats(&boot.model),
        real,
        "every real message, no arrival"
    );
    let read = boot
        .model
        .conversations
        .iter()
        .find(|c| c.peer == b.friends[0])
        .map(|c| c.unread_rows());
    assert_eq!(read, Some(0), "the reader's read stays");

    let (writes, erases) = (b.flash.writes, b.flash.erases);
    let again = home::forget(&mut b.flash, b.address, &|| ()).expect("again");
    assert_eq!((again.store.entries, again.summary_erased), (0, 0));
    assert_eq!(
        (b.flash.writes, b.flash.erases),
        (writes, erases),
        "nothing to do, nothing written"
    );
}
