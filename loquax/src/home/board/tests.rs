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
use crate::ui::strings::Lang;

#[derive(Clone)]
struct TestClock(Arc<AtomicU64>);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

type Node = NodeCore<OsRng, TestClock, MemoryStorage>;

fn node(clock: &TestClock, identity: Option<Identity>, hw_mtu: u32) -> Node {
    let mut builder = NodeCoreBuilder::new();
    if let Some(identity) = identity {
        builder = builder.identity(identity);
    }
    let mut node = builder.build(OsRng, clock.clone(), MemoryStorage::with_defaults());
    node.set_interface_name(0, std::string::String::from("wire"));
    node.set_interface_hw_mtu(0, hw_mtu);
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
    /// Answers this `/sync`, counted from 1, once, and the line loses
    /// every packet of the answer; the link stays up (1440).
    lose_at: Option<usize>,
    syncs: usize,
    /// The page each answered `/sync` asked, and its cursor.
    asked: Vec<Option<u64>>,
    cursors: Vec<u64>,
    /// What each `/sync` said the device folds.
    folds: Vec<wire::Fold>,
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
                let Ok((cursor, asked, fold)) = wire::read_sync_asked(data) else {
                    return wire::status_answer(Status::Malformed);
                };
                self.folds.push(fold);
                // As the home does (1439): only what the device folds.
                let keeps = |entry: &&wire::Entry| fold.keeps(entry.kind, entry.packed.len());
                self.asked.push(asked);
                self.cursors.push(cursor);
                let mut page = wire::Page {
                    entries: self
                        .archive
                        .iter()
                        .filter(|entry| entry.seq > cursor)
                        .filter(keeps)
                        .cloned()
                        .collect(),
                    more: false,
                    first: self.archive.first().map_or(0, |entry| entry.seq),
                    left: None,
                };
                if let Some(asked) = asked {
                    // As the home does (`server::sync`): count with the
                    // longest number, cut, then say how many wait.
                    page.left = Some(wire::LEFT_UNCOUNTED);
                    wire::fit_page(&mut page, asked as usize);
                    let after = page.entries.last().map_or(cursor, |entry| entry.seq);
                    let left = self
                        .archive
                        .iter()
                        .filter(|entry| entry.seq > after)
                        .filter(keeps);
                    page.left = Some(left.count() as u64);
                }
                wire::sync_answer(&page)
            }
            _ => wire::status_answer(Status::Malformed),
        }
    }
}

/// How a bench's ask ended, `None` when it ran out of time.
type Pulled = Option<Result<Outcome, HomeError>>;

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
        Self::on(archive, 564)
    }

    /// A bench whose two ends declare `hw_mtu`: 564 a serial line, 500
    /// what a LoRa path keeps.
    fn on(archive: Vec<wire::Entry>, hw_mtu: u32) -> Self {
        let clock = TestClock(Arc::new(AtomicU64::new(1_000_000)));
        let home_identity = Identity::generate(&mut OsRng);
        let public_key = home_identity.public_key_bytes();
        let mut home_node = node(&clock, None, hw_mtu);
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
            board: node(&clock, None, hw_mtu),
            clock,
            device: Identity::generate(&mut OsRng),
            home: Home {
                node: home_node,
                token,
                admitted: Vec::new(),
                archive,
                silent: false,
                break_at: None,
                lose_at: None,
                syncs: 0,
                asked: Vec::new(),
                cursors: Vec::new(),
                folds: Vec::new(),
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
            let lost = path == SYNC_PATH && self.home.lose_at == Some(self.home.syncs);
            if lost {
                self.home.lose_at = None;
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
            if !self.home.silent && !lost {
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

    /// A pull whose page the executor fits to the link (1439), as the
    /// M9's; the class and page it chose.
    fn pull_fitted(
        &mut self,
        record: &ClientRecord,
        batch: Batch,
    ) -> (Pulled, Option<(LinkClass, u64)>) {
        let ask = record
            .pull(batch.page_bytes, self.now(), 120_000)
            .expect("admitted")
            .expect("ask");
        let mut exec = Executor::fitted(ask, batch);
        let until = self.now() + 120_000;
        let pulled = self.run(&mut exec, until, 100);
        (pulled, exec.fit())
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

/// Unhappy path 1b (1440): the line loses the answer to the second
/// page and the link stays up, as the M9's UART lost a page under announce
/// traffic. The same request goes again on the same link once the
/// request's own bound (six round trips and 11.25 s) runs out, not at the
/// end of the pull's budget, and brings the same page: nothing twice,
/// nothing lost, one link.
#[test]
fn a_lost_page_on_a_live_link_is_asked_again_within_the_rtt_bound() {
    let archive: Vec<_> = (1..=6).map(|seq| entry(seq, 150)).collect();
    let mut bench = Bench::new(archive);
    let record = bench.join();
    bench.home.lose_at = Some(2);
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
    assert_eq!(bench.home.lose_at, None, "an answer was lost");
    // The repeat asked from the same cursor as the lost one.
    assert_eq!(bench.home.syncs, bench.pages.len() + 1);
    assert_eq!(bench.home.cursors[1], bench.home.cursors[2]);
    let took = bench.now() - start;
    let bound = leviculum_core::node::request::request_timeout_ms(
        leviculum_core::constants::CHANNEL_DEFAULT_RTT_MS,
    );
    assert!(
        took < bound + 5_000,
        "took {took} ms, the bound is {bound} ms; the pull's budget is 120 s"
    );
}

/// A join is never asked twice on its link: its token is spent by the
/// first ask that reaches the home, so a lost answer ends it.
#[test]
fn a_lost_join_answer_is_not_asked_again() {
    let mut record = {
        let bench = Bench::new(std::vec![]);
        ClientRecord::pending(&bench.device, &bench.code, "M9").expect("record")
    };
    let mut ask = record.join(0, 60_000).expect("pending").expect("ask");
    assert!(!ask.repeatable());
    assert!(matches!(ask.poll(0, Some(1)), Step::Connect { .. }));
    assert_eq!(ask.linked(Ok(())), Step::Identify);
    assert!(matches!(ask.identified(Ok(())), Step::Request { .. }));
    assert_eq!(ask.lost(), Step::Done(Err(HomeError::Timeout)));
    record.admission = Admission::Admitted;
    let mut pull = record.pull(64, 0, 60_000).expect("admitted").expect("ask");
    assert!(pull.repeatable());
    assert!(matches!(pull.poll(0, Some(1)), Step::Connect { .. }));
    assert_eq!(pull.linked(Ok(())), Step::Identify);
    let Step::Request { data: first, .. } = pull.identified(Ok(())) else {
        panic!("no request");
    };
    for _ in 0..crate::home::ask::REPEATS {
        let Step::Request { data, .. } = pull.lost() else {
            panic!("not asked again");
        };
        assert_eq!(data, first, "the repeat is the same request");
    }
    assert_eq!(pull.lost(), Step::Close, "past the repeats, a fresh link");
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

/// 1415: the pages say how many wait, so the board counts "N of M" as it
/// pulls, and a link broken mid-pull counts on from where it was, never
/// past M and never an entry twice.
#[test]
fn the_pull_counts_n_of_m_and_a_break_does_not_count_twice() {
    for break_at in [None, Some(2)] {
        let archive: Vec<_> = (1..=6).map(|seq| entry(seq, 150)).collect();
        let mut bench = Bench::new(archive);
        let record = bench.join();
        bench.home.break_at = break_at;
        let pulled = bench.pull(&record, lora());
        assert!(matches!(
            pulled,
            Some(Ok(Outcome::Pulled { entries: 6, .. }))
        ));
        let mut progress = Progress::default();
        let mut seen = Vec::new();
        for page in &bench.pages {
            progress.page(page);
            seen.push((progress.taken, progress.of));
        }
        assert!(seen.iter().all(|&(_, of)| of == Some(6)), "{seen:?}");
        assert_eq!(seen.last(), Some(&(6, Some(6))));
        assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "{seen:?}");
        let mut session = Session::new();
        session.finished(bench.now(), &pulled.expect("done"));
        assert_eq!(
            Word::of(&session, true, Some(progress))
                .text()
                .resolve(Lang::En),
            "Fetched 6 of 6"
        );
    }
}

/// 1415: the word the board's top bar shows for each state of its client,
/// in both languages; the M9 needed four words the catalogue lacked.
#[test]
fn the_word_follows_the_session() {
    let mut session = Session::new();
    let word = |session: &Session, admitted, progress| Word::of(session, admitted, progress);
    assert_eq!(word(&session, false, None), Word::Joining);
    assert_eq!(
        Word::Joining.text().resolve(Lang::De),
        "Verbinde mit deinem Postfach"
    );
    session.finished(1, &Err(HomeError::NoPath));
    assert_eq!(word(&session, false, None), Word::Unreachable);
    assert_eq!(word(&session, true, None), Word::Unreachable);
    assert_eq!(
        Word::Unreachable.text().resolve(Lang::En),
        "Mailbox unreachable"
    );
    session.finished(2, &Err(HomeError::Refused(Status::BadToken)));
    assert_eq!(word(&session, false, None), Word::CodeRefused);
    assert_eq!(
        Word::CodeRefused.text().resolve(Lang::En),
        "The mailbox did not take this code. Make a new one on the computer."
    );
    assert_eq!(word(&session, true, None), Word::Refused);
    session.finished(3, &Ok(Outcome::Joined));
    assert_eq!(word(&session, true, None), Word::Connected);
    let some = Progress { taken: 4, of: None };
    assert_eq!(
        word(&session, true, Some(some)).text().resolve(Lang::En),
        "Fetched 4"
    );
    let counted = Progress {
        taken: 4,
        of: Some(9),
    };
    assert_eq!(
        word(&session, true, Some(counted)).text().resolve(Lang::De),
        "4 von 9 abgeholt"
    );
    // Nothing taken is no news: the word stays "connected".
    assert_eq!(
        word(&session, true, Some(Progress::default())),
        Word::Connected
    );
}

/// 1415: a code as `loquax mailbox admit` prints it, with its rendezvous,
/// in capitals or broken over lines, becomes the pending record; a typo
/// and a foreign text are refused by name and make none.
#[test]
fn a_code_becomes_a_pending_record_and_a_bad_one_none() {
    let bench = Bench::new(Vec::new());
    let text = bench.code.encode();
    let record = admit(&text, &bench.device, "M9").expect("record");
    assert_eq!(record.home, bench.code.public_key);
    assert_eq!(
        record.admission,
        Admission::Pending {
            token: bench.code.token
        }
    );
    assert_eq!(record.name, "M9");
    let with_rendezvous = bench
        .code
        .encode_with_rendezvous(&[std::string::String::from("leviculum.network:4242")]);
    assert_eq!(
        admit(&with_rendezvous, &bench.device, "M9"),
        Ok(record.clone())
    );
    let shouted = std::format!(" {} \n", text.to_uppercase());
    assert_eq!(admit(&shouted, &bench.device, "M9"), Ok(record));
    let mut typo = text.clone();
    let other = if &typo[40..41] == "a" { "b" } else { "a" };
    typo.replace_range(40..41, other);
    assert_eq!(
        admit(&typo, &bench.device, "M9"),
        Err(AdmitError::Code(crate::home::code::CodeError::Checksum))
    );
    assert_eq!(
        admit("hello", &bench.device, "M9"),
        Err(AdmitError::Code(crate::home::code::CodeError::Prefix))
    );
}

/// 1415: what the board's model takes from the archive. A message the
/// home took in for its address is a received one, unchecked; one for
/// another address and bytes that are no message are nothing; a sent one
/// is the home's devices' bubble; a mark and a name pass as the engine
/// passes them; a kind the board does not show is nothing.
#[test]
fn entries_fold_into_what_the_model_takes() {
    use leviculum_lxmf::{DeliveryMethod, Message};
    let me = Identity::generate(&mut OsRng);
    let friend = Identity::generate(&mut OsRng);
    let address = crate::home::lxmf_address(&me.public_key_bytes()).expect("address");
    let theirs = crate::home::lxmf_address(&friend.public_key_bytes()).expect("address");
    let pack = |from: &Identity, to: [u8; 16], body: &str| {
        let source = crate::home::lxmf_address(&from.public_key_bytes()).expect("address");
        Message::create(
            to,
            source,
            from,
            1_762_000_000.0,
            std::vec::Vec::new(),
            body.as_bytes().to_vec(),
            std::vec::Vec::new(),
            DeliveryMethod::Direct,
        )
        .expect("create")
        .pack()
    };
    let at = |kind, packed| wire::Entry {
        seq: 1,
        kind,
        at: 1_762_000_000,
        packed,
    };
    let received = at(KIND_RECEIVED, pack(&friend, address, "hallo"));
    match fold(&received, &address) {
        Some(OutboxEvent::Received {
            source,
            body,
            verified,
            ..
        }) => {
            assert_eq!(source, theirs);
            assert_eq!(body, b"hallo");
            assert!(!verified, "the board checked no signature");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fold(&received, &theirs), None);
    assert_eq!(fold(&at(KIND_RECEIVED, std::vec![1, 2, 3]), &address), None);
    let sent = at(crate::home::KIND_SENT, pack(&me, theirs, "zurück"));
    assert!(matches!(
        fold(&sent, &address),
        Some(OutboxEvent::HomeSent { destination, ref body, .. }) if destination == theirs && body == "zurück".as_bytes()
    ));
    let mark = wire::Mark {
        flag: wire::MarkFlag::Read,
        ids: std::vec![[7; 32]],
    };
    assert_eq!(
        fold(
            &at(crate::home::KIND_MARK, wire::mark_request(&mark)),
            &address
        ),
        Some(OutboxEvent::HomeMarked { mark })
    );
    assert_eq!(
        fold(&at(crate::home::KIND_DELIVERED, std::vec![7; 32]), &address),
        None
    );
}

/// 1439: the page per link class. A radio keeps one response packet and
/// the census's page; a serial line without a rate gets a second of
/// 115200 baud; a wide link the device's answer up to the home's page;
/// a reported rate decides the radio and sizes the rest.
#[test]
fn the_page_follows_the_link_class() {
    use crate::home::ask::{LORA_LINK_MDU, SERIAL_LINK_MDU};
    let m9 = Batch {
        page_bytes: LORA_PAGE_BYTES as u64,
        max_answer: 256 * 1024,
    };
    assert_eq!(LinkClass::of(500, None), LinkClass::Radio);
    assert_eq!(
        LinkClass::of(508, None),
        LinkClass::Radio,
        "an RNode's HW_MTU"
    );
    assert_eq!(LinkClass::of(564, None), LinkClass::Serial);
    assert_eq!(LinkClass::of(1064, None), LinkClass::Wide, "UDP");
    assert_eq!(LinkClass::of(262_144, None), LinkClass::Wide, "TCP");
    assert_eq!(LinkClass::of(262_144, Some(3_125)), LinkClass::Radio, "SF8");
    assert_eq!(LinkClass::of(564, Some(115_200)), LinkClass::Serial);
    // LoRa: one packet, the LoRa page, 412.
    let lora = LinkClass::Radio.page_bytes(LORA_LINK_MDU, Some(3_125), m9);
    assert_eq!(lora, 412);
    // A census that found less room keeps its smaller page on a radio.
    let tight = Batch {
        page_bytes: 206,
        ..m9
    };
    assert_eq!(LinkClass::Radio.page_bytes(LORA_LINK_MDU, None, tight), 206);
    // Serial to the host's daemon: a second of the line, 11 520 B.
    let serial = LinkClass::Serial.page_bytes(SERIAL_LINK_MDU, None, m9);
    assert_eq!(serial, 11_520);
    assert_eq!(
        LinkClass::Serial.page_bytes(SERIAL_LINK_MDU, Some(921_600), m9),
        92_160.min(crate::home::SYNC_PAGE_BYTES as u64)
    );
    // TCP: the home's page, or less when the device takes less.
    assert_eq!(
        LinkClass::Wide.page_bytes(262_063, None, m9),
        crate::home::SYNC_PAGE_BYTES as u64
    );
    let small = Batch {
        max_answer: 4096,
        ..m9
    };
    assert_eq!(LinkClass::Wide.page_bytes(262_063, None, small), 4096);
    // A slow line still gets one packet at least.
    assert_eq!(
        LinkClass::Serial.page_bytes(SERIAL_LINK_MDU, Some(1_200), m9),
        (SERIAL_LINK_MDU - crate::home::ask::RESPONSE_OVERHEAD) as u64
    );
}

/// 1439: the executor fits a pull's page to the link it got. Over a
/// serial line's MTU the board asks a second of the line and the archive
/// comes in a handful of resources; over a link of the default MTU the
/// LoRa page of one packet. Each `/sync` says the board's word, and the
/// imported head and the delivery states never cross. Control: the same
/// archive pulled unfitted over the serial line asks the LoRa page.
#[test]
fn a_fitted_pull_asks_the_page_its_link_carries() {
    use crate::home::{KIND_DELIVERED, KIND_IMPORTED};
    let archive = || {
        let mut archive: Vec<_> = (1..=40)
            .map(|seq| wire::Entry {
                kind: KIND_IMPORTED,
                ..entry(seq, 150)
            })
            .collect();
        archive.extend((41..=70).map(|seq| entry(seq, 150)));
        archive.push(wire::Entry {
            kind: KIND_DELIVERED,
            ..entry(71, 40)
        });
        archive
    };
    let m9 = Batch {
        page_bytes: LORA_PAGE_BYTES as u64,
        max_answer: 256 * 1024,
    };
    let mut bench = Bench::on(archive(), 564);
    let record = bench.join();
    let (pulled, fit) = bench.pull_fitted(&record, m9);
    assert!(
        matches!(pulled, Some(Ok(Outcome::Pulled { entries: 30, .. }))),
        "{pulled:?}"
    );
    assert_eq!(fit, Some((LinkClass::Serial, 11_520)));
    assert_eq!(bench.seqs(), (41..=70).collect::<Vec<_>>());
    assert!(bench.home.asked.iter().all(|asked| *asked == Some(11_520)));
    assert_eq!(bench.home.asked.len(), 1, "thirty texts are one page");
    assert!(bench
        .home
        .folds
        .iter()
        .all(|fold| *fold == wire::Fold::Board));

    let mut bench = Bench::on(archive(), 500);
    let record = bench.join();
    let (pulled, fit) = bench.pull_fitted(&record, m9);
    assert!(matches!(
        pulled,
        Some(Ok(Outcome::Pulled { entries: 30, .. }))
    ));
    assert_eq!(fit, Some((LinkClass::Radio, LORA_PAGE_BYTES as u64)));
    assert_eq!(bench.home.asked.len(), 15, "two texts a LoRa page");

    let mut bench = Bench::on(archive(), 564);
    let record = bench.join();
    let pulled = bench.pull(&record, m9);
    assert!(matches!(
        pulled,
        Some(Ok(Outcome::Pulled { entries: 30, .. }))
    ));
    assert_eq!(bench.home.asked.len(), 15, "unfitted: the LoRa page");
}

/// 1439: the board's word names exactly the kinds `fold` reads; an entry
/// of any other kind folds to nothing, so the home skips nothing the
/// board would show.
#[test]
fn the_board_word_keeps_every_kind_fold_reads() {
    let address = [0x42; 16];
    for kind in 0..=u8::MAX {
        if wire::Fold::Board.keeps(kind, 0) {
            continue;
        }
        let entry = wire::Entry {
            seq: 1,
            kind,
            at: 0,
            packed: std::vec![0; 64],
        };
        assert_eq!(fold(&entry, &address), None, "kind {kind}");
    }
    for kind in wire::BOARD_KINDS {
        assert!(wire::Fold::Board.keeps(kind, wire::BOARD_ENTRY_MAX));
        assert!(!wire::Fold::Board.keeps(kind, wire::BOARD_ENTRY_MAX + 1));
    }
    assert_eq!(wire::Fold::from_word("board"), wire::Fold::Board);
    assert_eq!(wire::Fold::from_word("toaster"), wire::Fold::All);
    let data = wire::sync_request_folded(5, 412, wire::Fold::Board);
    assert_eq!(
        wire::read_sync_asked(&data),
        Ok((5, Some(412), wire::Fold::Board))
    );
    // A home from before reads the first two and serves everything.
    assert_eq!(wire::read_sync_paged(&data), Ok((5, Some(412))));
    let plain = wire::sync_request_folded(5, 412, wire::Fold::All);
    assert_eq!(plain, wire::sync_request_paged(5, 412));
    assert_eq!(
        wire::read_sync_asked(&wire::sync_request(5)),
        Ok((5, None, wire::Fold::All))
    );
}
