//! The M9's client as the board runs it (1414), against a real home: a
//! bare `leviculum-core` node driven by `loquax::home::board::Executor`,
//! on a byte line to a Rust transport node (lnsd's stack) with the
//! mailbox daemon behind it, the harness of 1363's `m9_client.rs`.
//!
//! The line is written the way the M9 writes its one UART: a debug line
//! before every frame (the `KBRAW` lines of the keyboard, 100 a second on
//! the board) and every frame with its XON and XOFF bytes escaped
//! (`leviculum_serial_mux::escape_flow`). The transport node's reader is
//! the core `Deframer`, as lnsd's serial interface is, so the test is also
//! the claim that the text between the frames costs the node nothing.

use std::time::Duration;

use leviculum_core::Identity;
use loquax::home::ask::{Ask, Outcome, Verb, LORA_PAGE_BYTES};
use loquax::home::board::Batch;
use loquax::home::code::HomeCode;
use loquax::home::record::{Admission, ClientRecord};
use loquax::home::wire;
use rand_core::OsRng;

#[path = "../../loquax/tests/support/reach.rs"]
mod reach;

use reach::{entry, logs, Mailbox, Relay};

#[path = "support/m9_board.rs"]
mod m9_board;

use m9_board::Board;

fn submission(content: &[u8]) -> wire::Submission {
    let mut client_id = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut OsRng, &mut client_id);
    wire::Submission {
        client_id,
        destination: [0x77; 16],
        title: Vec::new(),
        content: content.to_vec(),
        fields: Vec::new(),
    }
}

/// The board's record joins the real mailbox through the transport node,
/// sends three messages through it, and pulls the archive back in LoRa
/// pages, each page one packet; a second pull from the saved cursor
/// brings nothing twice. The line carried a debug line before every frame.
#[tokio::test(flavor = "multi_thread")]
async fn the_boards_core_node_joins_sends_and_pulls_from_a_real_home() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = Mailbox::start(vec![entry(relay.peer)]).await;
    let peer = relay.peer;
    let code = HomeCode {
        public_key: mailbox.public_key,
        token: mailbox.token,
    };
    let device = Identity::generate(&mut OsRng);
    let (record, joined, sent, pulled, again, pages, text_lines) =
        tokio::task::spawn_blocking(move || {
            let mut board = Board::connect(peer);
            let mut record = ClientRecord::pending(&device, &code, "M9").expect("record");
            let join = record
                .join(board.now(), 60_000)
                .expect("pending")
                .expect("ask");
            let (joined, _) = board.run(&device, join, 64 * 1024, Duration::from_secs(60));
            record.admission = Admission::Admitted;

            let mut sent = Vec::new();
            for text in [&b"Gruss vom M9"[..], b"zweite", b"dritte Nachricht"] {
                let ask = Ask::new(
                    &record.home,
                    Verb::Send(submission(text)),
                    board.now(),
                    30_000,
                )
                .expect("ask");
                sent.push(
                    board
                        .run(&device, ask, 64 * 1024, Duration::from_secs(30))
                        .0,
                );
            }

            let batch = Batch {
                page_bytes: LORA_PAGE_BYTES as u64,
                max_answer: 64 * 1024,
            };
            let ask = record
                .pull(batch.page_bytes, board.now(), 60_000)
                .expect("admitted")
                .expect("ask");
            let (pulled, pages) =
                board.run(&device, ask, batch.max_answer, Duration::from_secs(60));
            if let Some(Ok(Outcome::Pulled { cursor, .. })) = &pulled {
                record.cursor = *cursor;
            }
            let ask = record
                .pull(batch.page_bytes, board.now(), 60_000)
                .expect("admitted")
                .expect("ask");
            let (again, _) = board.run(&device, ask, batch.max_answer, Duration::from_secs(60));
            (record, joined, sent, pulled, again, pages, board.text_lines)
        })
        .await
        .expect("the board's thread");

    assert_eq!(joined, Some(Ok(Outcome::Joined)));
    assert!(
        sent.iter()
            .all(|sent| matches!(sent, Some(Ok(Outcome::Sent(_))))),
        "{sent:?}"
    );
    let Some(Ok(Outcome::Pulled {
        cursor, entries, ..
    })) = pulled
    else {
        panic!("not pulled: {pulled:?}");
    };
    assert!(entries >= 3, "the three sent messages: {entries}");
    assert_eq!(record.cursor, cursor);
    assert_eq!(
        pages.iter().map(|page| page.entries.len()).sum::<usize>(),
        entries
    );
    for page in &pages {
        assert!(
            wire::sync_answer(page).len() <= LORA_PAGE_BYTES,
            "a page is one packet"
        );
    }
    assert!(
        matches!(again, Some(Ok(Outcome::Pulled { entries: 0, .. }))),
        "{again:?}"
    );
    assert!(text_lines > 10, "debug lines between frames: {text_lines}");
    mailbox.stop().await;
}
