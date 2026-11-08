//! The cells of 1415: the M9 joining its home and pulling the archive,
//! happy and unhappy, staged on the host where the behaviour is shared;
//! and 1439's: the board pulls only what it folds, in pages its link
//! carries.
//!
//! A board cannot run on the host, and a host lnsd with the firmware's
//! client half does not exist, so each cell drives the client the board
//! runs (`loquax::home::board`: the executor over a bare core node, the
//! session, the word the top bar shows, the pull's count) on the line the
//! M9 writes (`support/m9_board.rs`), through a Rust transport node to the
//! real mailbox daemon. What only the board does (the keys, the panel,
//! flash, lnflash's frame) is the reviewer's recipe on the rig
//! (`docs/src/concepts/handheld-messenger.md`, "The M9 joins its home").
//!
//! Every cell prints one measure line, `[CELL] <name> key=value ...`, and
//! asserts on what that line reports.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use leviculum_core::Identity;
use loquax::home::ask::{HomeError, Outcome, LORA_PAGE_BYTES};
use loquax::home::board::{Batch, LinkClass, Progress, Session, Word};
use loquax::home::code::HomeCode;
use loquax::home::record::{Admission, ClientRecord};
use loquax::home::wire::{self, Status};
use loquax::home::{KIND_DELIVERED, KIND_IMPORTED, KIND_RECEIVED};
use loquax::ui::strings::Lang;
use rand_core::OsRng;

#[path = "../../loquax/tests/support/reach.rs"]
mod reach;

#[path = "support/m9_board.rs"]
mod m9_board;

use m9_board::{Board, Ran};
use reach::{entry, logs, Mailbox, Relay};

/// The token `Mailbox` issues, the phone's.
const PHONE: [u8; 16] = [0x42; 16];
/// A second device's token.
const SECOND: [u8; 16] = [0x43; 16];
/// A token issued two hours ago, past the mailbox's hour.
const EXPIRED: [u8; 16] = [0x55; 16];
/// A token the mailbox never issued.
const WRONG: [u8; 16] = [0x99; 16];

const BATCH: Batch = Batch {
    page_bytes: LORA_PAGE_BYTES as u64,
    max_answer: 64 * 1024,
};

/// A mailbox whose archive holds `entries` received entries of 150 bytes
/// each, and the second device's and the expired token beside the
/// phone's.
async fn mailbox(relay: &Relay, entries: usize) -> Mailbox {
    mailbox_with(relay, |store, now| {
        for n in 0..entries {
            let id: [u8; 32] = std::array::from_fn(|i| (n as u8) ^ i as u8);
            store
                .archive_append(KIND_RECEIVED, &id, &[n as u8; 150], now, 1 << 20)
                .expect("append")
                .expect("new");
        }
    })
    .await
}

/// A mailbox with the three tokens and the archive `fill` writes.
async fn mailbox_with(relay: &Relay, fill: impl FnOnce(&mut loquax::store::Store, i64)) -> Mailbox {
    let dir = tempfile::tempdir().expect("mailbox dir");
    let identity =
        loquax::identity::load_or_create(&dir.path().join("identity")).expect("identity");
    let address = loquax::engine::delivery_address(&identity).expect("address");
    let now = leviculum_std::local_time::unix_now_secs();
    {
        let mut store = loquax::store::Store::open_in(dir.path(), address).expect("store");
        store.issue_token(&SECOND, "second", now).expect("token");
        store
            .issue_token(&EXPIRED, "late", now - 2 * 3600)
            .expect("token");
        fill(&mut store, now);
    }
    Mailbox::start_in(dir, vec![entry(relay.peer)], true).await
}

fn code(mailbox: &Mailbox, token: [u8; 16]) -> HomeCode {
    HomeCode {
        public_key: mailbox.public_key,
        token,
    }
}

/// A board with a pending record for `code`.
fn pending(code: &HomeCode) -> (Identity, ClientRecord) {
    let device = Identity::generate(&mut OsRng);
    let record = ClientRecord::pending(&device, code, "M9").expect("record");
    (device, record)
}

fn join(board: &mut Board, device: &Identity, record: &ClientRecord, budget: Duration) -> Ran {
    let ask = record
        .join(board.now(), budget.as_millis() as u64)
        .expect("pending")
        .expect("ask");
    board.run(
        device,
        ask,
        BATCH.max_answer,
        budget + Duration::from_secs(5),
    )
}

fn pull(board: &mut Board, device: &Identity, record: &ClientRecord) -> Ran {
    let ask = record
        .pull(BATCH.page_bytes, board.now(), 120_000)
        .expect("admitted")
        .expect("ask");
    board.run(device, ask, BATCH.max_answer, Duration::from_secs(125))
}

/// What a pull's pages say: the sequences in the order they came, and
/// the count the board shows after each page.
fn counted(pages: &[wire::Page]) -> (Vec<u64>, Vec<Progress>) {
    let mut progress = Progress::default();
    let mut shown = Vec::new();
    let mut seqs = Vec::new();
    for page in pages {
        seqs.extend(page.entries.iter().map(|entry| entry.seq));
        progress.page(page);
        shown.push(progress);
    }
    (seqs, shown)
}

fn duplicates(seqs: &[u64]) -> usize {
    seqs.len() - seqs.iter().collect::<BTreeSet<_>>().len()
}

/// The word the top bar shows once `result` ended the ask.
fn word(result: &Result<Outcome, HomeError>, admitted: bool, progress: Option<Progress>) -> Word {
    let mut session = Session::new();
    session.finished(0, result);
    Word::of(&session, admitted, progress)
}

/// Happy: the board joins with a good code and pulls the archive in LoRa
/// pages, every entry once and in order, counting "N of M" with the right
/// M from the first page on.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_join_and_pull() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = mailbox(&relay, 24).await;
    let code = code(&mailbox, PHONE);
    let peer = relay.peer;
    let (joined, join_ms, pulled, pages, pull_ms) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        let (device, mut record) = pending(&code);
        let start = Instant::now();
        let (joined, _) = join(&mut board, &device, &record, Duration::from_secs(60));
        let join_ms = start.elapsed().as_millis();
        record.admission = Admission::Admitted;
        let start = Instant::now();
        let (pulled, pages) = pull(&mut board, &device, &record);
        (joined, join_ms, pulled, pages, start.elapsed().as_millis())
    })
    .await
    .expect("board");
    let (seqs, shown) = counted(&pages);
    let last = shown.last().copied().unwrap_or_default();
    let bytes: usize = pages.iter().map(|page| wire::sync_answer(page).len()).sum();
    let said = pulled
        .as_ref()
        .map(|pulled| word(pulled, true, Some(last)).text().resolve(Lang::En));
    println!(
        "[CELL] m9_join_and_pull joined={} join_ms={join_ms} taken={} of={:?} pages={} answer_bytes={bytes} dup={} pull_ms={pull_ms} word={said:?}",
        matches!(joined, Some(Ok(Outcome::Joined))),
        last.taken,
        last.of,
        pages.len(),
        duplicates(&seqs),
    );
    assert_eq!(joined, Some(Ok(Outcome::Joined)));
    assert!(
        matches!(pulled, Some(Ok(Outcome::Pulled { entries: 24, .. }))),
        "{pulled:?}"
    );
    assert_eq!(seqs.len(), 24);
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    assert!(shown.iter().all(|p| p.of == Some(24)), "{shown:?}");
    assert!(
        pages.len() >= 8,
        "two 150 B entries to a LoRa page: {}",
        pages.len()
    );
    assert!(pages
        .iter()
        .all(|page| wire::sync_answer(page).len() <= LORA_PAGE_BYTES));
    assert_eq!(said.as_deref(), Some("Fetched 24 of 24"));
    mailbox.stop().await;
}

/// Unhappy: a code the mailbox never issued, and one it issued two hours
/// ago with tokens living an hour, are both refused by name; the board
/// says the mailbox did not take the code and asks for a new one, and
/// stays unadmitted.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_wrong_and_expired_code() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = mailbox(&relay, 0).await;
    let peer = relay.peer;
    let codes = [
        ("wrong", code(&mailbox, WRONG)),
        ("expired", code(&mailbox, EXPIRED)),
    ];
    let ran = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        codes
            .into_iter()
            .map(|(name, code)| {
                let (device, record) = pending(&code);
                let start = Instant::now();
                let (joined, _) = join(&mut board, &device, &record, Duration::from_secs(60));
                (name, joined, start.elapsed().as_millis())
            })
            .collect::<Vec<_>>()
    })
    .await
    .expect("board");
    for (name, joined, ms) in ran {
        let said = joined
            .as_ref()
            .map(|joined| word(joined, false, None).text().resolve(Lang::En));
        println!("[CELL] m9_{name}_code result={joined:?} ms={ms} word={said:?}");
        assert_eq!(
            joined,
            Some(Err(HomeError::Refused(Status::BadToken))),
            "{name}"
        );
        assert_eq!(
            said.as_deref(),
            Some("The mailbox did not take this code. Make a new one on the computer.")
        );
    }
    mailbox.stop().await;
}

/// Unhappy: no mailbox behind the line at all. The join asks for a path
/// on the ladder until its budget is spent and ends with an error, not a
/// panic and not a hang; the board says the mailbox is unreachable and
/// waits 30 s before it asks again.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_home_unreachable_at_join() {
    logs();
    let relay = Relay::start(&[]).await;
    let absent = HomeCode {
        public_key: Identity::generate(&mut OsRng).public_key_bytes(),
        token: PHONE,
    };
    let peer = relay.peer;
    let (joined, ms) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        let (device, record) = pending(&absent);
        let start = Instant::now();
        let (joined, _) = join(&mut board, &device, &record, Duration::from_secs(25));
        (joined, start.elapsed().as_millis())
    })
    .await
    .expect("board");
    let mut session = Session::new();
    let result = joined.clone().expect("the ask ended inside its budget");
    session.finished(0, &result);
    let said = Word::of(&session, false, None).text().resolve(Lang::En);
    println!(
        "[CELL] m9_home_unreachable result={joined:?} ms={ms} word={said:?} next_ask_ms={}",
        session.next_ms()
    );
    assert!(
        matches!(result, Err(HomeError::NoPath | HomeError::Timeout)),
        "{result:?}"
    );
    assert!(ms < 31_000, "{ms} ms");
    assert_eq!(said, "Mailbox unreachable");
    assert_eq!(session.next_ms(), 30_000);
    let _ = relay.stop().await;
}

/// Unhappy: the line is cut after the second page and dialled again, the
/// way a pulled cable or a restarted lnsd cuts it. The pull goes on from
/// the last entry it took on a fresh link: every entry once, none lost,
/// and the count never passes M.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_link_breaks_mid_pull() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = mailbox(&relay, 24).await;
    let code = code(&mailbox, PHONE);
    let peer = relay.peer;
    let (pulled, pages, cuts, ms, after_cut) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        let (device, mut record) = pending(&code);
        let (joined, _) = join(&mut board, &device, &record, Duration::from_secs(60));
        assert_eq!(joined, Some(Ok(Outcome::Joined)));
        record.admission = Admission::Admitted;
        board.cut_after_pages = Some(2);
        let start = Instant::now();
        let start_ms = board.now();
        let (pulled, pages) = pull(&mut board, &device, &record);
        let after: Vec<String> = board
            .after_cut
            .iter()
            .map(|(at, name)| format!("{}:{name}", at.saturating_sub(start_ms)))
            .collect();
        (
            pulled,
            pages,
            board.cuts,
            start.elapsed().as_millis(),
            after,
        )
    })
    .await
    .expect("board");
    let (seqs, shown) = counted(&pages);
    let last = shown.last().copied().unwrap_or_default();
    println!(
        "[CELL] m9_link_breaks_mid_pull cuts={cuts} taken={} of={:?} pages={} dup={} pull_ms={ms} result={pulled:?}",
        last.taken,
        last.of,
        pages.len(),
        duplicates(&seqs),
    );
    println!("[CELL] m9_link_breaks_mid_pull events_after_cut={after_cut:?}");
    assert_eq!(cuts, 1);
    assert!(
        matches!(pulled, Some(Ok(Outcome::Pulled { entries: 24, .. }))),
        "{pulled:?}"
    );
    assert_eq!(duplicates(&seqs), 0, "{seqs:?}");
    assert_eq!(seqs.len(), 24);
    assert!(
        shown.iter().all(|p| p.of == Some(24) && p.taken <= 24),
        "{shown:?}"
    );
    mailbox.stop().await;
}

/// Unhappy: a second device joins with its own code while the first is
/// mid-pull. Neither disturbs the other: both pulls end, and after one
/// more pull from each cursor both boards hold every entry the archive
/// holds, each once.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_second_device_joins_mid_pull() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = mailbox(&relay, 40).await;
    let (first, second) = (code(&mailbox, PHONE), code(&mailbox, SECOND));
    let peer = relay.peer;
    let paged = Arc::new(AtomicBool::new(false));
    let flag = paged.clone();
    let one = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        let (device, mut record) = pending(&first);
        let (joined, _) = join(&mut board, &device, &record, Duration::from_secs(60));
        assert_eq!(joined, Some(Ok(Outcome::Joined)));
        record.admission = Admission::Admitted;
        board.paged = Some(flag);
        let (pulled, mut pages) = pull(&mut board, &device, &record);
        if let Some(Ok(Outcome::Pulled { cursor, .. })) = &pulled {
            record.cursor = *cursor;
        }
        let (again, more) = pull(&mut board, &device, &record);
        pages.extend(more);
        (pulled, again, pages)
    });
    let two = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        while !paged.load(Ordering::SeqCst) {
            assert!(
                start.elapsed() < Duration::from_secs(90),
                "the first never paged"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut board = Board::connect(peer);
        let (device, mut record) = pending(&second);
        let (joined, _) = join(&mut board, &device, &record, Duration::from_secs(60));
        record.admission = Admission::Admitted;
        let (pulled, mut pages) = pull(&mut board, &device, &record);
        if let Some(Ok(Outcome::Pulled { cursor, .. })) = &pulled {
            record.cursor = *cursor;
        }
        let (again, more) = pull(&mut board, &device, &record);
        pages.extend(more);
        (joined, pulled, again, pages)
    });
    let (pulled_one, again_one, pages_one) = one.await.expect("first board");
    let (joined_two, pulled_two, again_two, pages_two) = two.await.expect("second board");
    let archive: Vec<u64> =
        loquax::store::Store::open_in(mailbox.dir.path(), *mailbox.delivery().as_bytes())
            .expect("store")
            .archive_since(0, usize::MAX)
            .expect("archive")
            .entries
            .iter()
            .map(|entry| entry.seq)
            .collect();
    let (seqs_one, _) = counted(&pages_one);
    let (seqs_two, _) = counted(&pages_two);
    println!(
        "[CELL] m9_second_device_mid_pull archive={} first={} first_dup={} second={} second_dup={} second_joined={:?}",
        archive.len(),
        seqs_one.len(),
        duplicates(&seqs_one),
        seqs_two.len(),
        duplicates(&seqs_two),
        joined_two,
    );
    assert_eq!(joined_two, Some(Ok(Outcome::Joined)));
    for result in [&pulled_one, &again_one, &pulled_two, &again_two] {
        assert!(
            matches!(result, Some(Ok(Outcome::Pulled { .. }))),
            "{result:?}"
        );
    }
    assert_eq!(duplicates(&seqs_one), 0);
    assert_eq!(duplicates(&seqs_two), 0);
    assert!(archive.len() >= 40, "{}", archive.len());
    assert_eq!(seqs_one, archive);
    assert_eq!(seqs_two, archive);
    mailbox.stop().await;
}

/// 1439, happy and unhappy in one archive: an imported head of 60
/// entries, then 24 texts, a delivery state and a photo over the board's
/// bound. The board's fitted pull over the serial line's MTU asks a second
/// of the line, says `board`, and takes exactly the 24 texts in one page,
/// "Fetched 24 of 24" from that page on: the head, the state and the
/// photo never cross. Control: the same board pulling unfitted asks the
/// LoRa page and needs a dozen pages for the same 24.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_board_pulls_what_it_folds_in_pages_of_its_link() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = mailbox_with(&relay, |store, now| {
        let mut put = |kind: u8, n: u32, len: usize| {
            let mut id = [kind; 32];
            id[..4].copy_from_slice(&n.to_be_bytes());
            store
                .archive_append(kind, &id, &vec![n as u8; len], now, 1 << 24)
                .expect("append")
                .expect("new")
        };
        for n in 0..60 {
            put(KIND_IMPORTED, n, 150);
        }
        for n in 0..24 {
            put(KIND_RECEIVED, n, 150);
        }
        put(KIND_DELIVERED, 0, 40);
        put(KIND_RECEIVED, 1_000, wire::BOARD_ENTRY_MAX + 1);
    })
    .await;
    let code = code(&mailbox, PHONE);
    let peer = relay.peer;
    let m9 = Batch {
        page_bytes: LORA_PAGE_BYTES as u64,
        max_answer: 256 * 1024,
    };
    let (fitted, fit, fitted_ms, lora) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        let (device, mut record) = pending(&code);
        let (joined, _) = join(&mut board, &device, &record, Duration::from_secs(60));
        assert_eq!(joined, Some(Ok(Outcome::Joined)));
        record.admission = Admission::Admitted;
        let ask = record
            .pull(m9.page_bytes, board.now(), 120_000)
            .expect("admitted")
            .expect("ask");
        let start = Instant::now();
        let (fitted, fit) = board.run_fitted(&device, ask, m9, Duration::from_secs(125));
        let fitted_ms = start.elapsed().as_millis();
        // The control: the same record, unfitted, from its first cursor.
        let lora = pull(&mut board, &device, &record);
        (fitted, fit, fitted_ms, lora)
    })
    .await
    .expect("board");
    let (pulled, pages) = fitted;
    let (seqs, shown) = counted(&pages);
    let last = shown.last().copied().unwrap_or_default();
    let said = pulled
        .as_ref()
        .map(|pulled| word(pulled, true, Some(last)).text().resolve(Lang::En));
    let (lora_pulled, lora_pages) = lora;
    let (lora_seqs, _) = counted(&lora_pages);
    println!(
        "[CELL] m9_board_folds fit={fit:?} taken={} of={:?} pages={} first_seq={:?} pull_ms={fitted_ms} word={said:?} lora_pages={} lora_taken={}",
        last.taken,
        last.of,
        pages.len(),
        seqs.first(),
        lora_pages.len(),
        lora_seqs.len(),
    );
    assert_eq!(fit, Some((LinkClass::Serial, 11_520)));
    assert!(
        matches!(pulled, Some(Ok(Outcome::Pulled { entries: 24, .. }))),
        "{pulled:?}"
    );
    // The head is seq 1..=60, the texts 61..=84, the state 85, the photo 86.
    assert_eq!(seqs, (61..=84).collect::<Vec<u64>>());
    assert!(shown.iter().all(|p| p.of == Some(24)), "{shown:?}");
    assert_eq!(pages.len(), 1, "24 texts are one serial page");
    assert_eq!(said.as_deref(), Some("Fetched 24 of 24"));
    assert!(matches!(
        lora_pulled,
        Some(Ok(Outcome::Pulled { entries: 24, .. }))
    ));
    assert_eq!(lora_seqs, seqs);
    assert!(
        lora_pages.len() >= 12,
        "two texts a LoRa page: {}",
        lora_pages.len()
    );
    mailbox.stop().await;
}

/// A mailbox whose archive holds a short text, then one text longer than
/// a link packet (1459, the M9's stall of 2026-11-06: the entry after its
/// cursor 77 did not fit one packet). Repeated bytes, which bz2 squeezes
/// to a fraction: a home that compressed its answers would send this one
/// compressed.
async fn long_entry_mailbox(relay: &Relay) -> Mailbox {
    mailbox_with(relay, |store, now| {
        for (n, len) in [(0u8, 150), (1, 900)] {
            store
                .archive_append(KIND_RECEIVED, &[n; 32], &vec![n; len], now, 1 << 20)
                .expect("append")
                .expect("new");
        }
    })
    .await
}

/// Join, then pull fitted from cursor 1, as the M9 with the long entry
/// after its cursor; the pull, its fit and how long it took.
fn pull_after_short(board: &mut Board, code: &HomeCode) -> (Ran, Option<(LinkClass, u64)>, u128) {
    let m9 = Batch {
        page_bytes: LORA_PAGE_BYTES as u64,
        max_answer: 256 * 1024,
    };
    let (device, mut record) = pending(code);
    let (joined, _) = join(board, &device, &record, Duration::from_secs(60));
    assert_eq!(joined, Some(Ok(Outcome::Joined)));
    record.admission = Admission::Admitted;
    record.cursor = 1;
    // Timed on the board's own clock, from the instant the ask's deadline
    // is anchored to: the ask times out at `now_ms >= asked_at + 120_000`
    // (`loquax/src/home/ask.rs`), so a timed-out pull reads at least
    // 120 000 here. A wall-clock start taken after the ask was built read
    // 119 999 ms when the board's millisecond floor and the gap between
    // the two clocks fell together (order 1520's workspace run).
    let asked_at = board.now();
    let ask = record
        .pull(m9.page_bytes, asked_at, 120_000)
        .expect("admitted")
        .expect("ask");
    let (ran, fit) = board.run_fitted(&device, ask, m9, Duration::from_secs(125));
    (ran, fit, u128::from(board.now().saturating_sub(asked_at)))
}

/// 1459, happy: the entry after the board's cursor is longer than a link
/// packet, so the mailbox answers the serial page as a resource. The
/// board, which links no decompressor, takes it: the entry once, no
/// cancel on the line, one page.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_long_entry_comes_as_a_resource() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = long_entry_mailbox(&relay).await;
    let code = code(&mailbox, PHONE);
    let peer = relay.peer;
    let (ran, fit, pull_ms, parts, refusals) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        let (ran, fit, pull_ms) = pull_after_short(&mut board, &code);
        (ran, fit, pull_ms, board.parts, board.refusals)
    })
    .await
    .expect("board");
    let (pulled, pages) = ran;
    let (seqs, _) = counted(&pages);
    println!(
        "[CELL] m9_long_entry fit={fit:?} pulled={pulled:?} seqs={seqs:?} pages={} parts={parts} refusals={refusals} pull_ms={pull_ms}",
        pages.len()
    );
    assert_eq!(fit, Some((LinkClass::Serial, 11_520)));
    assert!(
        matches!(
            pulled,
            Some(Ok(Outcome::Pulled {
                cursor: 2,
                entries: 1,
                ..
            }))
        ),
        "{pulled:?}"
    );
    assert_eq!(seqs, vec![2]);
    assert!(parts >= 2, "the answer came in parts: {parts}");
    assert_eq!(refusals, 0, "the board cancelled its own answer");
    mailbox.stop().await;
}

/// 1459, unhappy: the board's line loses the first parts of the resource
/// that answers its `/sync`, as the M9's UART lost frames under traffic
/// (1440). The board asks the missing parts again on the same link, the
/// entry comes once, and the pull ends within the request bound of 1440,
/// not at the end of its 120 s budget.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_long_entry_parts_lost_mid_way() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = long_entry_mailbox(&relay).await;
    let code = code(&mailbox, PHONE);
    let peer = relay.peer;
    let (ran, fit, pull_ms, lost, refusals, rtt) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        board.lose_parts = 2;
        let (ran, fit, pull_ms) = pull_after_short(&mut board, &code);
        let rtt = leviculum_core::constants::CHANNEL_DEFAULT_RTT_MS;
        (ran, fit, pull_ms, board.parts_lost, board.refusals, rtt)
    })
    .await
    .expect("board");
    let (pulled, pages) = ran;
    let (seqs, _) = counted(&pages);
    let bound = leviculum_core::node::request::request_timeout_ms(rtt);
    println!(
        "[CELL] m9_long_entry_parts_lost fit={fit:?} pulled={pulled:?} seqs={seqs:?} lost={lost} refusals={refusals} pull_ms={pull_ms} bound_ms={bound}"
    );
    assert_eq!(fit, Some((LinkClass::Serial, 11_520)));
    assert_eq!(lost, 2, "the line lost two parts");
    assert!(
        matches!(
            pulled,
            Some(Ok(Outcome::Pulled {
                cursor: 2,
                entries: 1,
                ..
            }))
        ),
        "{pulled:?}"
    );
    assert_eq!(seqs, vec![2], "the entry once");
    assert_eq!(refusals, 0);
    assert!(
        pull_ms < u128::from(bound),
        "took {pull_ms} ms, the bound is {bound} ms"
    );
    mailbox.stop().await;
}

/// 1461, unhappy: the line loses every part of the resource that answers
/// the board's `/sync`, retransmissions too, while the link stays up, as
/// 1459 measured on the bench. The resource's watchdog keeps asking for
/// the parts and would run past the pull's budget; the board cancels the
/// silent answer within the request bound of 1440 (one RCL) and asks the
/// page again on the same link, where it arrives once.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_long_entry_answer_lost_whole() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = long_entry_mailbox(&relay).await;
    let code = code(&mailbox, PHONE);
    let peer = relay.peer;
    let (ran, fit, pull_ms, lost, refusals) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        board.vanish_answer = true;
        let (ran, fit, pull_ms) = pull_after_short(&mut board, &code);
        (ran, fit, pull_ms, board.parts_lost, board.refusals)
    })
    .await
    .expect("board");
    let (pulled, pages) = ran;
    let (seqs, _) = counted(&pages);
    let bound = leviculum_core::node::request::request_timeout_ms(
        leviculum_core::constants::CHANNEL_DEFAULT_RTT_MS,
    );
    println!(
        "[CELL] m9_long_entry_answer_lost_whole fit={fit:?} pulled={pulled:?} seqs={seqs:?} lost={lost} refusals={refusals} pull_ms={pull_ms} bound_ms={bound}"
    );
    assert_eq!(fit, Some((LinkClass::Serial, 11_520)));
    assert!(lost >= 2, "the line lost the answer's parts: {lost}");
    assert!(
        matches!(
            pulled,
            Some(Ok(Outcome::Pulled {
                cursor: 2,
                entries: 1,
                ..
            }))
        ),
        "{pulled:?}"
    );
    assert_eq!(seqs, vec![2], "the entry once");
    assert_eq!(refusals, 1, "the board cancelled the silent answer, once");
    assert!(
        pull_ms < u128::from(bound) + 5_000,
        "took {pull_ms} ms, the bound is {bound} ms"
    );
    mailbox.stop().await;
}

/// 1461, the control: the same loss without the rule. The watchdog asks
/// for the parts again and again; when its retries run out (about 67 s on
/// this line, measured 2026-11-07) the request fails and 1440 asks the
/// page again, but the home's sender still holds the abandoned answer and
/// refuses every new one ("resource transfer already in progress"), so
/// the pull's 120 s run out: no page, no cancel. The rule's RCL is what
/// frees the home's sender.
#[tokio::test(flavor = "multi_thread")]
async fn m9_cell_long_entry_answer_lost_whole_without_the_rule() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = long_entry_mailbox(&relay).await;
    let code = code(&mailbox, PHONE);
    let peer = relay.peer;
    let (ran, pull_ms, lost, refusals) = tokio::task::spawn_blocking(move || {
        let mut board = Board::connect(peer);
        board.vanish_answer = true;
        board.answer_watch_off = true;
        let (ran, _, pull_ms) = pull_after_short(&mut board, &code);
        (ran, pull_ms, board.parts_lost, board.refusals)
    })
    .await
    .expect("board");
    let (pulled, pages) = ran;
    println!(
        "[CELL] m9_long_entry_answer_lost_whole_control pulled={pulled:?} pages={} lost={lost} refusals={refusals} pull_ms={pull_ms}",
        pages.len()
    );
    assert_eq!(pulled, Some(Err(HomeError::Timeout)), "after {pull_ms} ms");
    assert!(pages.is_empty());
    assert_eq!(refusals, 0);
    assert!(pull_ms >= 120_000, "took {pull_ms} ms");
    mailbox.stop().await;
}
