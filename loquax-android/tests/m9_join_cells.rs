//! The cells of 1415: the M9 joining its home and pulling the archive,
//! happy and unhappy, staged on the host where the behaviour is shared.
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
use loquax::home::board::{Batch, Progress, Session, Word};
use loquax::home::code::HomeCode;
use loquax::home::record::{Admission, ClientRecord};
use loquax::home::wire::{self, Status};
use loquax::home::KIND_RECEIVED;
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
        for n in 0..entries {
            let id: [u8; 32] = std::array::from_fn(|i| (n as u8) ^ i as u8);
            store
                .archive_append(KIND_RECEIVED, &id, &[n as u8; 150], now, 1 << 20)
                .expect("append")
                .expect("new");
        }
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
