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

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use leviculum_core::framing::hdlc::{self, DeframeResult, Deframer};
use leviculum_core::node::{NodeCore, NodeCoreBuilder};
use leviculum_core::traits::{Clock, Interface, InterfaceError};
use leviculum_core::transport::dispatch_actions;
use leviculum_core::{Identity, InterfaceId, MemoryStorage, TickOutput};
use loquax::home::ask::{Ask, Outcome, Verb, LORA_PAGE_BYTES};
use loquax::home::board::{Batch, Executor, Report};
use loquax::home::code::HomeCode;
use loquax::home::record::{Admission, ClientRecord};
use loquax::home::wire;
use rand_core::OsRng;

#[path = "../../loquax/tests/support/reach.rs"]
mod reach;

use reach::{entry, logs, Mailbox, Relay};

/// The board's clock: milliseconds since the board came up.
#[derive(Clone)]
struct Uptime(Instant);

impl Clock for Uptime {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The board's serial interface: a queue the loop writes out.
#[derive(Default)]
struct Line(Vec<Vec<u8>>);

impl Interface for Line {
    fn id(&self) -> InterfaceId {
        InterfaceId(0)
    }
    fn name(&self) -> &str {
        "serial"
    }
    fn mtu(&self) -> usize {
        564
    }
    fn is_online(&self) -> bool {
        true
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        self.0.push(data.to_vec());
        Ok(())
    }
}

/// The board: its node, its line to the transport node, and what it wrote.
struct Board {
    node: Box<NodeCore<OsRng, Uptime, MemoryStorage>>,
    line: Line,
    stream: TcpStream,
    deframer: Deframer,
    clock: Uptime,
    /// Debug lines written between frames.
    text_lines: usize,
    /// Frames that carried an escaped flow byte.
    flow_escaped: usize,
}

impl Board {
    fn connect(addr: SocketAddr) -> Self {
        let clock = Uptime(Instant::now());
        let mut node = NodeCoreBuilder::new().enable_transport(true).build_boxed(
            OsRng,
            clock.clone(),
            MemoryStorage::with_defaults(),
        );
        node.set_interface_name(0, "serial".to_string());
        node.set_interface_hw_mtu(0, 564);
        node.set_interface_mode(0, leviculum_core::InterfaceMode::Gateway);
        let stream = TcpStream::connect(addr).expect("the transport node's port");
        stream
            .set_read_timeout(Some(Duration::from_millis(5)))
            .expect("timeout");
        let mut board = Self {
            node,
            line: Line::default(),
            stream,
            deframer: Deframer::new(),
            clock,
            text_lines: 0,
            flow_escaped: 0,
        };
        let up = board.node.handle_interface_up(0);
        board.dispatch(up);
        board
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn dispatch(&mut self, out: TickOutput) -> Vec<leviculum_core::node::NodeEvent> {
        let mut ifaces: [&mut dyn Interface; 1] = [&mut self.line];
        let dispatched = dispatch_actions(&mut ifaces, out.actions, &Default::default());
        self.node.record_dispatch_drops(&dispatched);
        out.events
    }

    /// Write what the node queued as the M9 writes it: a debug line, then
    /// the frame with its flow bytes escaped.
    fn flush(&mut self) {
        for packet in std::mem::take(&mut self.line.0) {
            let line = format!("KBRAW {} 0000000000000000\r\n", self.now());
            self.stream.write_all(line.as_bytes()).expect("write");
            self.text_lines += 1;
            let mut framed = Vec::new();
            hdlc::frame(&packet, &mut framed);
            let mut wire = Vec::new();
            leviculum_serial_mux::escape_flow(&framed, &mut |piece| wire.extend_from_slice(piece));
            if wire.len() != framed.len() {
                self.flow_escaped += 1;
            }
            self.stream.write_all(&wire).expect("write");
        }
    }

    /// Perform `ask` as the M9's loop does, until it is done or `budget`
    /// passes; the pages it brought.
    fn run(
        &mut self,
        device: &Identity,
        ask: Ask,
        max_answer: usize,
        budget: Duration,
    ) -> (
        Option<Result<Outcome, loquax::home::ask::HomeError>>,
        Vec<wire::Page>,
    ) {
        let mut exec = Executor::new(ask, max_answer);
        let mut pages = Vec::new();
        let until = Instant::now() + budget;
        let mut buf = [0u8; 4096];
        while Instant::now() < until {
            let mut outs = Vec::new();
            match self.stream.read(&mut buf) {
                Ok(0) => panic!("the transport node closed the line"),
                Ok(n) => {
                    for result in self.deframer.process(&buf[..n]) {
                        if let DeframeResult::Frame(frame) = result {
                            outs.push(self.node.handle_packet(InterfaceId(0), &frame));
                        }
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => panic!("read: {error}"),
            }
            if self
                .node
                .next_deadline()
                .is_some_and(|due| due <= self.now())
            {
                outs.push(self.node.handle_timeout());
            }
            let mut out = TickOutput::default();
            let now = self.now();
            let mut reports = vec![exec.poll(&mut self.node, device, now, &mut out)];
            outs.push(out);
            while let Some(out) = outs.pop() {
                for event in self.dispatch(out) {
                    let mut more = TickOutput::default();
                    let now = self.now();
                    reports.push(exec.event(&mut self.node, device, &event, now, &mut more));
                    outs.push(more);
                }
            }
            self.flush();
            for report in reports {
                match report {
                    Report::Idle => {}
                    Report::Page(page) => pages.push(page),
                    Report::Done(result) => return (Some(result), pages),
                }
            }
        }
        (None, pages)
    }
}

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
