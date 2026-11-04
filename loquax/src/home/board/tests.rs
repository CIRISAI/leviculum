//! The board's executor against a fake home on a second core node, wired
//! back to back in memory (1414): a join, a pull in LoRa pages, and the
//! three ways a pull goes wrong on a board, a link that breaks mid-pull,
//! a home that answers nothing, and an answer the board cannot take.
//!
//! The fake home answers `/join` and `/sync` the way `server` does
//! (`fit_page` on the asked page, a refusal for a device not admitted),
//! from entries in memory; the client side is exactly what the M9 runs.

extern crate std;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::vec::Vec;

use leviculum_core::node::{NodeCore, NodeCoreBuilder, NodeEvent, RequestPolicy};
use leviculum_core::traits::Clock;
use leviculum_core::{Action, Identity, InterfaceId, LinkId, MemoryStorage, ProofStrategy};
use rand_core::OsRng;

use super::*;
use crate::home::ask::{Ask, Verb, ATTEMPTS, LORA_PAGE_BYTES};
use crate::home::code::HomeCode;
use crate::home::record::{Admission, ClientRecord};
use crate::home::{JOIN_PATH, KIND_RECEIVED, SYNC_PATH};

#[derive(Clone)]
struct TestClock(Arc<AtomicU64>);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

type Node = NodeCore<OsRng, TestClock, MemoryStorage>;

fn node(clock: &TestClock, identity: Option<Identity>) -> Node {
    let mut builder = NodeCoreBuilder::new();
    if let Some(identity) = identity {
        builder = builder.identity(identity);
    }
    let mut node = builder.build(OsRng, clock.clone(), MemoryStorage::with_defaults());
    node.set_interface_name(0, std::string::String::from("wire"));
    node.set_interface_hw_mtu(0, 564);
    node
}

/// The home: its node, its token, the devices it admitted and its archive.
struct Home {
    node: Node,
    token: [u8; 16],
    admitted: Vec<[u8; 16]>,
    archive: Vec<wire::Entry>,
    /// Drops every packet it would send: a home that answers nothing.
    silent: bool,
    /// Closes the link instead of answering this `/sync`, counted from 1,
    /// once.
    break_at: Option<usize>,
    syncs: usize,
    /// The page each answered `/sync` asked, and its cursor.
    asked: Vec<Option<u64>>,
    cursors: Vec<u64>,
}

impl Home {
    fn answer(&mut self, link: &LinkId, path: &str, data: &[u8]) -> Vec<u8> {
        let device = self
            .node
            .get_remote_identity(link)
            .map(|identity| *identity.hash());
        let Some(device) = device else {
            return wire::status_answer(Status::NotIdentified);
        };
        match path {
            JOIN_PATH => match wire::read_join_request(data) {
                Ok((token, _)) if token == self.token => {
                    self.token = [0; 16];
                    self.admitted.push(device);
                    wire::status_answer(Status::Ok)
                }
                Ok(_) => wire::status_answer(Status::BadToken),
                Err(_) => wire::status_answer(Status::Malformed),
            },
            SYNC_PATH => {
                if !self.admitted.contains(&device) {
                    return wire::status_answer(Status::NotAdmitted);
                }
                let Ok((cursor, asked)) = wire::read_sync_paged(data) else {
                    return wire::status_answer(Status::Malformed);
                };
                self.asked.push(asked);
                self.cursors.push(cursor);
                let mut page = wire::Page {
                    entries: self
                        .archive
                        .iter()
                        .filter(|entry| entry.seq > cursor)
                        .cloned()
                        .collect(),
                    more: false,
                    first: self.archive.first().map_or(0, |entry| entry.seq),
                };
                if let Some(asked) = asked {
                    wire::fit_page(&mut page, asked as usize);
                }
                wire::sync_answer(&page)
            }
            _ => wire::status_answer(Status::Malformed),
        }
    }
}

/// Both nodes, the clock, and what the board has taken in.
struct Bench {
    clock: TestClock,
    board: Node,
    device: Identity,
    home: Home,
    code: HomeCode,
    /// Every page the board was handed.
    pages: Vec<wire::Page>,
    /// Packets in flight each way.
    to_home: Vec<Vec<u8>>,
    to_board: Vec<Vec<u8>>,
}

fn entry(seq: u64, len: usize) -> wire::Entry {
    wire::Entry {
        seq,
        kind: KIND_RECEIVED,
        at: 1_762_000_000 + seq as i64,
        packed: std::vec![seq as u8; len],
    }
}

impl Bench {
    fn new(archive: Vec<wire::Entry>) -> Self {
        let clock = TestClock(Arc::new(AtomicU64::new(1_000_000)));
        let home_identity = Identity::generate(&mut OsRng);
        let public_key = home_identity.public_key_bytes();
        let mut home_node = node(&clock, None);
        let mut destination = crate::home::destination(home_identity).expect("home destination");
        destination.set_accepts_links(true);
        destination.set_proof_strategy(ProofStrategy::All);
        let hash = *destination.hash();
        home_node.register_destination(destination);
        for path in [JOIN_PATH, SYNC_PATH] {
            home_node.register_request_handler(hash, path, RequestPolicy::AllowAll);
        }
        let token = [0x5A; 16];
        Self {
            board: node(&clock, None),
            clock,
            device: Identity::generate(&mut OsRng),
            home: Home {
                node: home_node,
                token,
                admitted: Vec::new(),
                archive,
                silent: false,
                break_at: None,
                syncs: 0,
                asked: Vec::new(),
                cursors: Vec::new(),
            },
            code: HomeCode { public_key, token },
            pages: Vec::new(),
            to_home: Vec::new(),
            to_board: Vec::new(),
        }
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn sends(out: &TickOutput) -> impl Iterator<Item = Vec<u8>> + '_ {
        out.actions.iter().map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
    }

    /// The home's side of one output: its packets onto the wire, its
    /// requests answered.
    fn home_output(&mut self, out: TickOutput) {
        if !self.home.silent {
            self.to_board.extend(Self::sends(&out));
        }
        for event in out.events {
            let NodeEvent::RequestReceived {
                link_id,
                request_id,
                path,
                data,
                ..
            } = event
            else {
                continue;
            };
            if path == SYNC_PATH {
                self.home.syncs += 1;
                if self.home.break_at == Some(self.home.syncs) {
                    // The link goes down with the request in flight: no
                    // answer, the close packet instead.
                    self.home.break_at = None;
                    let closed = self.home.node.close_link(&link_id);
                    self.to_board.extend(Self::sends(&closed));
                    continue;
                }
            }
            let answer = self.home.answer(&link_id, &path, &data);
            let sent = if self.home.node.response_fits_packet(&link_id, answer.len()) {
                self.home
                    .node
                    .send_response(&link_id, &request_id, &answer)
                    .expect("response")
            } else {
                self.home
                    .node
                    .send_response_resource(&link_id, &request_id, &answer)
                    .expect("resource response")
                    .1
            };
            if !self.home.silent {
                self.to_board.extend(Self::sends(&sent));
            }
        }
    }

    /// The board's side of one output: packets out, events to the
    /// executor, whose own output goes the same way.
    fn board_output(&mut self, exec: &mut Executor, out: TickOutput) -> Vec<Report> {
        let mut reports = Vec::new();
        let mut pending = std::vec![out];
        while let Some(out) = pending.pop() {
            self.to_home.extend(Self::sends(&out));
            for event in &out.events {
                let mut more = TickOutput::default();
                let now = self.now();
                let report = exec.event(&mut self.board, &self.device, event, now, &mut more);
                reports.push(report);
                pending.push(more);
            }
        }
        reports
    }

    /// Run the board's ask until it is done or `until_ms` passes, moving
    /// the clock by `step_ms` whenever nothing is in flight.
    fn run(
        &mut self,
        exec: &mut Executor,
        until_ms: u64,
        step_ms: u64,
    ) -> Option<Result<Outcome, HomeError>> {
        loop {
            let mut reports = Vec::new();
            let mut out = TickOutput::default();
            let now = self.now();
            reports.push(exec.poll(&mut self.board, &self.device, now, &mut out));
            reports.extend(self.board_output(exec, out));
            for _ in 0..64 {
                if self.to_home.is_empty() && self.to_board.is_empty() {
                    break;
                }
                for packet in core::mem::take(&mut self.to_home) {
                    let out = self.home.node.handle_packet(InterfaceId(0), &packet);
                    self.home_output(out);
                }
                for packet in core::mem::take(&mut self.to_board) {
                    let out = self.board.handle_packet(InterfaceId(0), &packet);
                    reports.extend(self.board_output(exec, out));
                }
            }
            for report in reports {
                match report {
                    Report::Idle => {}
                    Report::Page(page) => self.pages.push(page),
                    Report::Done(result) => return Some(result),
                }
            }
            if self.now() >= until_ms {
                return None;
            }
            self.clock.0.fetch_add(step_ms, Ordering::SeqCst);
            let out = self.home.node.handle_timeout();
            self.home_output(out);
            let out = self.board.handle_timeout();
            let reports = self.board_output(exec, out);
            for report in reports {
                if let Report::Done(result) = report {
                    return Some(result);
                }
                if let Report::Page(page) = report {
                    self.pages.push(page);
                }
            }
        }
    }

    /// Join with the code, as the record's pending join does.
    fn join(&mut self) -> ClientRecord {
        let mut record = ClientRecord::pending(&self.device, &self.code, "M9").expect("record");
        let ask = record
            .join(self.now(), 60_000)
            .expect("pending")
            .expect("ask");
        let mut exec = Executor::new(ask, 4096);
        let until = self.now() + 60_000;
        let joined = self.run(&mut exec, until, 100);
        assert_eq!(joined, Some(Ok(Outcome::Joined)));
        record.admission = Admission::Admitted;
        record
    }

    fn pull(&mut self, record: &ClientRecord, batch: Batch) -> Option<Result<Outcome, HomeError>> {
        let ask = record
            .pull(batch.page_bytes, self.now(), 120_000)
            .expect("admitted")
            .expect("ask");
        let mut exec = Executor::new(ask, batch.max_answer);
        let until = self.now() + 120_000;
        self.run(&mut exec, until, 100)
    }

    fn seqs(&self) -> Vec<u64> {
        self.pages
            .iter()
            .flat_map(|page| page.entries.iter().map(|entry| entry.seq))
            .collect()
    }
}

fn lora() -> Batch {
    Batch {
        page_bytes: LORA_PAGE_BYTES as u64,
        max_answer: 4096,
    }
}

/// The happy path: the board joins with the code, then pulls the archive
/// in LoRa pages of one packet each, every entry once and in order.
#[test]
fn the_board_joins_and_pulls_the_archive_in_lora_pages() {
    let archive: Vec<_> = (1..=6).map(|seq| entry(seq, 150)).collect();
    let mut bench = Bench::new(archive);
    let mut record = bench.join();
    assert_eq!(bench.home.admitted, std::vec![*bench.device.hash()]);
    let pulled = bench.pull(&record, lora());
    let Some(Ok(Outcome::Pulled {
        cursor,
        entries,
        pages,
        ..
    })) = pulled
    else {
        panic!("not pulled: {pulled:?}");
    };
    assert_eq!((cursor, entries), (6, 6));
    assert!(
        pages >= 3,
        "150-byte entries come two to a LoRa page: {pages}"
    );
    assert_eq!(bench.seqs(), (1..=6).collect::<Vec<_>>());
    assert!(bench
        .home
        .asked
        .iter()
        .all(|asked| *asked == Some(LORA_PAGE_BYTES as u64)));

    // The cursor goes into the record; the next pull asks from it and
    // brings nothing twice.
    record.cursor = cursor;
    bench.pages.clear();
    let again = bench.pull(&record, lora());
    assert!(matches!(
        again,
        Some(Ok(Outcome::Pulled {
            cursor: 6,
            entries: 0,
            ..
        }))
    ));
    assert!(bench.seqs().is_empty());
}

/// A device the home did not admit is refused by name, and the session
/// shows it as not connected.
#[test]
fn a_pull_before_the_join_is_refused_and_shown() {
    let mut bench = Bench::new(std::vec![entry(1, 40)]);
    let mut record = ClientRecord::pending(&bench.device, &bench.code, "M9").expect("record");
    record.admission = Admission::Admitted;
    let result = bench.pull(&record, lora());
    assert_eq!(result, Some(Err(HomeError::Refused(Status::NotAdmitted))));
    let mut session = Session::new();
    session.finished(bench.now(), &Err(HomeError::Refused(Status::NotAdmitted)));
    assert_eq!(session.reach(), Reach::Refused(Status::NotAdmitted));
    assert_eq!(session.notice(), Some(StrId::MailboxNotConnected));
}

/// Unhappy path 1: the link breaks after the first page. The same ask
/// opens a fresh link and goes on from the last entry it took, so nothing
/// comes twice and nothing is lost.
#[test]
fn a_link_that_breaks_mid_pull_resumes_from_the_cursor() {
    let archive: Vec<_> = (1..=6).map(|seq| entry(seq, 150)).collect();
    let mut bench = Bench::new(archive);
    let record = bench.join();
    bench.home.break_at = Some(2);
    let start = bench.now();
    let pulled = bench.pull(&record, lora());
    assert!(
        matches!(
            pulled,
            Some(Ok(Outcome::Pulled {
                cursor: 6,
                entries: 6,
                ..
            }))
        ),
        "{pulled:?}"
    );
    assert_eq!(bench.seqs(), (1..=6).collect::<Vec<_>>());
    assert_eq!(bench.home.break_at, None, "the link was broken");
    // The second link asked from where the first page ended: the home saw
    // one `/sync` more than the pages it answered, the one it dropped,
    // and the request after the break carried the first page's last.
    let first_page_last = bench.pages[0].entries.last().expect("entries").seq;
    assert!(first_page_last < 6);
    assert_eq!(bench.home.syncs, bench.pages.len() + 1);
    assert_eq!(bench.home.cursors[1], first_page_last);
    // The break is noticed when the close arrives, not when the request
    // would have timed out.
    assert!(
        bench.now() - start < 10_000,
        "took {} ms",
        bench.now() - start
    );
}

/// Unhappy path 2: the home answers nothing. The path ladder asks inside
/// the ask, the ask ends with no path, and the session waits longer after
/// each ask that does not get through, showing the home as not
/// connected; one that gets through resets it.
#[test]
fn a_home_that_answers_nothing_backs_off_and_shows_unreachable() {
    let mut bench = Bench::new(std::vec![entry(1, 40)]);
    bench.home.silent = true;
    let record = ClientRecord::pending(&bench.device, &bench.code, "M9").expect("record");
    let ask = record
        .join(bench.now(), 60_000)
        .expect("pending")
        .expect("ask");
    let mut exec = Executor::new(ask, 4096);
    let until = bench.now() + 61_000;
    let result = bench.run(&mut exec, until, 500);
    assert_eq!(result, Some(Err(HomeError::NoPath)));
    assert!(
        exec.ask().path_requests() >= 4,
        "the ladder asked {} times in a minute",
        exec.ask().path_requests()
    );

    let mut session = Session::new();
    assert!(session.due(0));
    assert_eq!(session.notice(), None);
    let mut now = 0;
    let mut waits = Vec::new();
    for _ in 0..7 {
        session.finished(now, &result.clone().expect("ended"));
        waits.push(session.next_ms() - now);
        assert!(!session.due(now));
        now = session.next_ms();
    }
    assert_eq!(
        waits,
        [30_000, 60_000, 120_000, 240_000, 480_000, 900_000, 900_000]
    );
    assert_eq!(session.reach(), Reach::Unreachable);
    assert_eq!(session.notice(), Some(StrId::MailboxNotConnected));
    assert_eq!(session.failures(), 7);
    session.hurry(now - 1);
    assert!(session.due(now - 1));

    let ok = Ok(Outcome::Joined);
    session.finished(now, &ok);
    assert_eq!(session.notice(), Some(StrId::MailboxConnected));
    session.finished(now, &Err(HomeError::Timeout));
    assert_eq!(session.next_ms() - now, BACKOFF_FIRST_MS);
}

/// Unhappy path 3: the answers are larger than the board takes. The
/// board asks the same page smaller on the same link until one fits, and
/// still gets every entry; an entry that fits no page ends the pull with
/// an error, not a panic and not a skipped message.
#[test]
fn an_answer_that_does_not_fit_asks_a_smaller_page_and_never_panics() {
    let archive: Vec<_> = (1..=4).map(|seq| entry(seq, 90)).collect();
    let mut bench = Bench::new(archive);
    let record = bench.join();
    let small = Batch {
        page_bytes: LORA_PAGE_BYTES as u64,
        max_answer: 150,
    };
    let pulled = bench.pull(&record, small);
    assert!(
        matches!(
            pulled,
            Some(Ok(Outcome::Pulled {
                cursor: 4,
                entries: 4,
                ..
            }))
        ),
        "{pulled:?}"
    );
    assert_eq!(bench.seqs(), [1, 2, 3, 4]);
    assert!(
        bench
            .home
            .asked
            .iter()
            .any(|asked| asked.is_some_and(|asked| asked < LORA_PAGE_BYTES as u64)),
        "a smaller page was asked: {:?}",
        bench.home.asked
    );

    let mut bench = Bench::new(std::vec![entry(1, 600)]);
    let record = bench.join();
    let pulled = bench.pull(&record, small);
    assert!(
        matches!(pulled, Some(Err(HomeError::Transfer(_)))),
        "{pulled:?}"
    );
    assert!(bench.seqs().is_empty(), "nothing taken past the cursor");
}

/// The batch from the census: the LoRa page when the room holds its
/// peak, a proportionally smaller page when it does not, never under the
/// floor, and halving stops there.
#[test]
fn the_batch_follows_the_census_and_has_a_floor() {
    let lora = LORA_PAGE_BYTES as u64;
    assert_eq!(Batch::sized(392, 1_000, 8192).page_bytes, lora);
    assert_eq!(Batch::sized(1_000, 1_000, 8192).page_bytes, lora);
    assert_eq!(Batch::sized(2_000, 1_000, 8192).page_bytes, lora / 2);
    assert_eq!(Batch::sized(100_000, 10, 8192).page_bytes, MIN_PAGE_BYTES);
    assert_eq!(Batch::sized(392, 1_000, 8192).max_answer, 8192);
    assert_eq!(Batch::halved(412), Some(206));
    assert_eq!(Batch::halved(100), Some(64));
    assert_eq!(Batch::halved(MIN_PAGE_BYTES), None);
}

/// The ask a refit is for: a pull's page shrinks and is asked again on
/// the same link; anything else out of turn ends the ask.
#[test]
fn a_refit_asks_the_same_page_smaller_and_only_a_pull() {
    let home = Identity::generate(&mut OsRng).public_key_bytes();
    let verb = Verb::Pull {
        cursor: 9,
        page_bytes: Some(412),
    };
    let mut ask = Ask::new(&home, verb, 0, 60_000).expect("ask");
    assert!(matches!(ask.poll(0, Some(1)), Step::Connect { .. }));
    ask.linked(Ok(()));
    ask.identified(Ok(()));
    let Step::Request { data, .. } = ask.refit(206) else {
        panic!("no request");
    };
    assert_eq!(wire::read_sync_paged(&data), Ok((9, Some(206))));
    assert!(matches!(
        ask.refit(206),
        Step::Done(Err(HomeError::Transfer(_)))
    ));
    assert_eq!(ATTEMPTS, 3);
}
