//! 1575: the UI core's drain yields. In window 16b (2026-11-08) one drain
//! turn of the M9 folded a pulled page of 234 entries in 836 ms before it
//! looked at the keys again, and a key pressed into it waited for all of
//! it. Here the turns of the board's loop are played over a clock the test
//! moves: each entry folded costs [`ENTRY_US`] (836 ms / 234, the 16b
//! turn's figure), a press is asked after each entry, and the drain folds
//! through [`home::Folding`] as `ui_main` does. Counted: the longest turn,
//! how long a key waits, the pages counted folded, the rows handed to the
//! summary log, and the model the turns leave against one that took each
//! page whole. The positive control is the same loop with no budget, as
//! before 1575: its longest turn is the page.

use std::cell::Cell;
use std::collections::VecDeque;

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::listing::Row;
use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::home::{self, Folding, FOLD_BUDGET_US};
use rand_core::OsRng;

/// What one entry costs the board's UI core: 836 ms for 234 (16b).
const ENTRY_US: u64 = 3_600;
/// The 16b page.
const PAGE: usize = 234;

fn entries(me: &Identity, address: [u8; 16], friends: &[Identity], n: u64) -> Vec<Entry> {
    (1..=n)
        .map(|seq| {
            let friend = &friends[seq as usize % friends.len()];
            let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
            let at = 1_762_000_000.0 + seq as f64;
            let (kind, from, to) = match seq % 5 {
                0 => (KIND_SENT, me, theirs),
                _ => (KIND_RECEIVED, friend, address),
            };
            let source = lxmf_address(&from.public_key_bytes()).expect("address");
            let packed = Message::create(
                to,
                source,
                from,
                at,
                Vec::new(),
                format!("Nachricht {seq}").into_bytes(),
                Vec::new(),
                DeliveryMethod::Direct,
            )
            .expect("create")
            .pack();
            Entry {
                seq,
                kind,
                at: at as i64,
                packed,
            }
        })
        .collect()
}

/// The board's UI loop, its drain alone, over a clock the test moves.
struct Loop {
    model: UiModel<Field>,
    address: [u8; 16],
    folding: Folding,
    /// What the I/O core handed over and the loop has not taken yet.
    queue: VecDeque<Vec<Entry>>,
    clock: Cell<u64>,
    /// The presses' times, oldest first; a press is queued once the
    /// clock passed it.
    presses: VecDeque<u64>,
    /// How long each key waited from its press to the end of the drain
    /// that took it.
    waits: Vec<u64>,
    /// Every turn's drain, in microseconds.
    turns: Vec<u64>,
    /// The pages counted folded, and the `Rows` messages sent.
    pages: u32,
    sent: Vec<(Vec<Row>, u32)>,
    budget: Option<u64>,
}

impl Loop {
    fn new(model: UiModel<Field>, address: [u8; 16], budget: Option<u64>) -> Self {
        Self {
            model,
            address,
            folding: Folding::default(),
            queue: VecDeque::new(),
            clock: Cell::new(0),
            presses: VecDeque::new(),
            waits: Vec::new(),
            turns: Vec::new(),
            pages: 0,
            sent: Vec::new(),
            budget,
        }
    }

    fn key_waiting(&self) -> bool {
        self.presses
            .front()
            .is_some_and(|&at| at <= self.clock.get())
    }

    /// One turn: the drain as `ui_main`'s, then the keys and the rows.
    fn turn(&mut self) {
        let start = self.clock.get();
        let deadline = self.budget.map_or(u64::MAX, |b| start + b);
        let mut rows = Vec::new();
        let mut gone = Vec::new();
        let mut pages = 0;
        loop {
            if self.folding.busy() {
                let clock = &self.clock;
                let presses = &self.presses;
                let budgeted = self.budget.is_some();
                let chunk =
                    self.folding
                        .fold(&mut self.model, &|| clock.get(), deadline, &mut || {
                            // The entry's cost, then the press asked.
                            clock.set(clock.get() + ENTRY_US);
                            budgeted && presses.front().is_some_and(|&at| at <= clock.get())
                        });
                let routed = home::route(&mut self.model, chunk.effects, &mut || [7; 16]);
                rows.retain(|row: &Row| !routed.rows.iter().any(|new| new.peer == row.peer));
                rows.extend(routed.rows);
                gone.extend(routed.gone);
                if chunk.done.is_some() {
                    pages += 1;
                    break;
                }
                if self.folding.busy() {
                    break;
                }
            }
            if self.clock.get() >= deadline || (self.budget.is_some() && self.key_waiting()) {
                break;
            }
            match self.queue.pop_front() {
                Some(page) => self.folding.begin(page, self.address),
                None => break,
            }
        }
        self.turns.push(self.clock.get() - start);
        // The keys: every press the clock passed.
        while self.key_waiting() {
            let at = self.presses.pop_front().expect("a press");
            self.waits.push(self.clock.get() - at);
        }
        if self.folding.rows_due(&mut rows, &mut gone) && (!rows.is_empty() || pages > 0) {
            self.sent.push((rows, pages));
        }
        self.pages += pages;
        // A turn that did nothing lets the clock run to the next press.
        if self.turns.last() == Some(&0) {
            if let Some(&at) = self.presses.front() {
                self.clock.set(self.clock.get().max(at));
            }
        }
    }

    fn run(&mut self) {
        while !self.queue.is_empty() || self.folding.busy() || !self.presses.is_empty() {
            self.turn();
        }
    }
}

fn shape(model: &UiModel<Field>) -> Vec<([u8; 16], usize, usize)> {
    let mut out: Vec<_> = model
        .conversations
        .iter()
        .map(|c| (c.peer, c.messages.len(), c.unread))
        .collect();
    out.sort();
    out
}

#[test]
fn a_drain_turn_folds_a_budget_and_a_key_waits_no_longer() {
    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..12).map(|_| Identity::generate(&mut OsRng)).collect();
    let all = entries(&me, address, &friends, 2 * PAGE as u64);
    let pages: Vec<Vec<Entry>> = all.chunks(PAGE).map(<[Entry]>::to_vec).collect();
    // A press every 37 ms over the pull, none on a turn's edge by design.
    let presses: VecDeque<u64> = (1..40).map(|n| n * 37_000 + 1).collect();

    // The control: each page taken whole, as `home::take` takes it.
    let mut whole = home::model(address);
    for page in &pages {
        home::take(&mut whole, page, &address);
    }

    let mut budgeted = Loop::new(home::model(address), address, Some(FOLD_BUDGET_US));
    budgeted.queue.extend(pages.iter().cloned());
    budgeted.presses = presses.clone();
    budgeted.run();

    let mut before = Loop::new(home::model(address), address, None);
    before.queue.extend(pages.iter().cloned());
    before.presses = presses;
    before.run();

    let longest = |l: &Loop| l.turns.iter().copied().max().unwrap_or(0);
    let worst = |l: &Loop| l.waits.iter().copied().max().unwrap_or(0);
    println!(
        "DRAIN budget_us={FOLD_BUDGET_US} entry_us={ENTRY_US} turns={} longest_us={} key_wait_max_us={} keys={}",
        budgeted.turns.len(),
        longest(&budgeted),
        worst(&budgeted),
        budgeted.waits.len()
    );
    println!(
        "DRAIN before turns={} longest_us={} key_wait_max_us={} keys={}",
        before.turns.len(),
        longest(&before),
        worst(&before),
        before.waits.len()
    );

    // The positive control: without the budget a turn is the page.
    assert_eq!(longest(&before), PAGE as u64 * ENTRY_US);
    assert!(
        worst(&before) > 4 * FOLD_BUDGET_US,
        "before: {}",
        worst(&before)
    );
    // With it no turn folds past the budget by more than the entry that
    // crossed it.
    assert!(
        longest(&budgeted) <= FOLD_BUDGET_US + ENTRY_US,
        "longest turn {} us",
        longest(&budgeted)
    );
    // A key waits for the rest of the turn it fell into and the one
    // entry the next folds before it yields.
    assert_eq!(budgeted.waits.len(), 39, "every press answered");
    assert!(
        worst(&budgeted) <= FOLD_BUDGET_US + 2 * ENTRY_US,
        "a key waited {} us",
        worst(&budgeted)
    );
    // The pages counted once each, the summary's rows sent once a page.
    assert_eq!(budgeted.pages, 2);
    assert_eq!(budgeted.sent.len(), 2, "one Rows a page");
    assert_eq!(
        budgeted.sent.iter().map(|(_, p)| p).sum::<u32>(),
        2,
        "pages owed paid once each"
    );
    // The turns leave the model the whole pages leave.
    assert_eq!(shape(&budgeted.model), shape(&whole));
    // Every conversation the pages touched has its row said.
    let said: std::collections::BTreeSet<[u8; 16]> = budgeted
        .sent
        .iter()
        .flat_map(|(rows, _)| rows.iter().map(|r| r.peer))
        .collect();
    assert_eq!(said.len(), whole.conversations.len());
}

#[test]
fn a_waiting_key_stops_the_fold_after_one_entry() {
    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..3).map(|_| Identity::generate(&mut OsRng)).collect();
    let page = entries(&me, address, &friends, 20);
    let mut model = home::model(address);
    let mut folding = Folding::default();
    folding.begin(page, address);
    let chunk = folding.fold(&mut model, &|| 0, u64::MAX, &mut || true);
    assert_eq!(chunk.looked, 1, "a press waits: one entry, then the keys");
    assert!(folding.busy());
    // The negative: no press, no deadline, the page folds whole.
    let chunk = folding.fold(&mut model, &|| 0, u64::MAX, &mut || false);
    assert_eq!(chunk.looked, 19);
    assert!(chunk.done.is_some_and(|done| done.len() == 20));
    assert!(!folding.busy());
}

#[test]
fn rows_wait_for_the_page_and_the_newest_row_wins() {
    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..2).map(|_| Identity::generate(&mut OsRng)).collect();
    let page = entries(&me, address, &friends, 10);
    let mut model = home::model(address);
    let mut folding = Folding::default();
    folding.begin(page, address);
    let chunk = folding.fold(&mut model, &|| 0, u64::MAX, &mut || true);
    let mut rows = home::route(&mut model, chunk.effects, &mut || [7; 16]).rows;
    let first = rows.clone();
    assert!(!first.is_empty());
    let mut gone = Vec::new();
    assert!(!folding.rows_due(&mut rows, &mut gone), "held mid-page");
    assert!(rows.is_empty(), "the held rows are the fold's now");
    let chunk = folding.fold(&mut model, &|| 0, u64::MAX, &mut || false);
    assert!(chunk.done.is_some());
    let mut rows = home::route(&mut model, chunk.effects, &mut || [7; 16]).rows;
    assert!(folding.rows_due(&mut rows, &mut gone), "the page is folded");
    // One row a conversation, the newest: the first chunk's row of the
    // same peer replaced.
    let peers: std::collections::BTreeSet<_> = rows.iter().map(|r| r.peer).collect();
    assert_eq!(peers.len(), rows.len());
    for row in &first {
        let now = rows
            .iter()
            .find(|r| r.peer == row.peer)
            .expect("still said");
        assert!(now.unread >= row.unread);
    }
}
