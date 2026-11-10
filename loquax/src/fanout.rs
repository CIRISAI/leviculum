//! One engine, several readers: the TUI's screen and the send or fetch
//! runner working for it.
//!
//! The runners ([`crate::send::run_send_auto`], [`crate::fetch::run_fetch`])
//! each take an [`Outbox`] and read its events to the end of their run, as
//! they do for the CLI. The TUI runs them unchanged, the same code the CLI's
//! `send` and `fetch` run, while its screen reads the same events: a
//! delivery glyph moves on the very `State` event the runner waits for, and a
//! message received mid-send must reach the screen and not die in the
//! runner's ignore arm. So the shell keeps the engine's outbox to itself and
//! hands each runner a [`Tap`]: every engine event is copied to every live
//! tap and returned to the screen, and a tap's commands go to the engine.
//!
//! The engine says `Ready` once per process and [`crate::send::run_send`]
//! waits for it before it resolves, so a tap opened later is handed the
//! `Ready` the fan-out kept.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};

/// How a frontend folds a backlog of the engine's events (1446, 1464):
/// at most `chunk` between two looks at the clock and at whether to stop,
/// and no more after `budget`. The first pull of a big mailbox queues
/// thousands, each a store commit when folded: folded in one go they took
/// 12.8 s on the host, while the phone's screens waited on its lock and
/// the window's shell answered nothing.
#[derive(Clone, Copy, Debug)]
pub struct Slice {
    pub chunk: usize,
    pub budget: Duration,
}

/// How the frontends fold: one event at a time, 50 ms a turn. With eight
/// a turn ran 60 to 80 ms past its budget on the host (1464).
pub const SLICE: Slice = Slice {
    chunk: 1,
    budget: Duration::from_millis(50),
};

/// One slice of a frontend's turn (1464): [`Slicing::next`] hands it the
/// engine's events a [`Slice::chunk`] at a time, as [`Fanout::pump_at_most`]
/// takes them, until the queue is empty, the budget is spent, or the
/// frontend says to stop; [`Slicing::backlog`] then says whether events
/// still wait, for the frontend to come back soon, after its screens had
/// their turn. The frontend folds each chunk itself, between the calls.
///
/// The budget is spent on a clock: the wall's for the frontends
/// ([`Slicing::new`]), one the caller owns with [`Slicing::on`] (1623: a
/// test's folds advance its own clock, where a slept millisecond on a
/// loaded host overran the budget's slack and read "a round took 47 ms").
pub struct Slicing {
    slice: Slice,
    clock: Box<dyn Fn() -> Duration + Send>,
    began: Duration,
    /// Whether the last chunk was full; none before the first.
    full: Option<bool>,
    backlog: bool,
}

impl Slicing {
    /// A slice on the wall's clock.
    pub fn new(slice: Slice) -> Self {
        let start = Instant::now();
        Self::on(slice, Box::new(move || start.elapsed()))
    }

    /// A slice whose budget is spent on `clock`: the time since any fixed
    /// start, read when the slice begins and before each chunk.
    pub fn on(slice: Slice, clock: Box<dyn Fn() -> Duration + Send>) -> Self {
        let began = clock();
        Self {
            slice: Slice {
                chunk: slice.chunk.max(1),
                ..slice
            },
            clock,
            began,
            full: None,
            backlog: false,
        }
    }

    /// The next chunk to fold, or `None` once the slice is over; `stop`
    /// (a screen waits) ends it after the chunk folded last.
    pub fn next(
        &mut self,
        fanout: &mut Fanout,
        outbox: &mut impl Outbox,
        stop: bool,
    ) -> Result<Option<Vec<OutboxEvent>>, OutboxGone> {
        match self.full {
            Some(false) => return Ok(None),
            Some(true)
                if stop || (self.clock)().saturating_sub(self.began) >= self.slice.budget =>
            {
                self.backlog = true;
                return Ok(None);
            }
            _ => {}
        }
        let events = fanout.pump_at_most(outbox, self.slice.chunk)?;
        self.full = Some(events.len() == self.slice.chunk);
        Ok(Some(events))
    }

    /// The slice ended with events still waiting in the engine's queue.
    pub fn backlog(&self) -> bool {
        self.backlog
    }
}

/// The shell's side: the taps' queues and the `Ready` to replay.
pub struct Fanout {
    taps: Vec<Sender<OutboxEvent>>,
    commands_tx: Sender<Command>,
    /// The taps' commands; lent with the engine's outbox to a
    /// [`Reader`] while one reads on a thread of its own.
    commands_rx: Option<Receiver<Command>>,
    ready: Option<OutboxEvent>,
}

impl Default for Fanout {
    fn default() -> Self {
        let (commands_tx, commands_rx) = channel();
        Self {
            taps: Vec::new(),
            commands_tx,
            commands_rx: Some(commands_rx),
            ready: None,
        }
    }
}

impl Fanout {
    /// A reader for one runner, already holding the engine's `Ready` when
    /// it has been said.
    pub fn tap(&mut self) -> Tap {
        let (events_tx, events_rx) = channel();
        if let Some(ready) = &self.ready {
            // The receiver is in hand: this cannot fail.
            let _ = events_tx.send(ready.clone());
        }
        self.taps.push(events_tx);
        Tap {
            commands: self.commands_tx.clone(),
            events: events_rx,
        }
    }

    /// Hand the taps' commands to the engine, then take every event waiting
    /// there: each is copied to every live tap and returned for the screen.
    /// A tap whose runner has finished is dropped here.
    pub fn pump(&mut self, outbox: &mut impl Outbox) -> Result<Vec<OutboxEvent>, OutboxGone> {
        self.pump_at_most(outbox, usize::MAX)
    }

    /// [`Fanout::pump`], taking at most `max` events; the rest wait in the
    /// engine's queue for the next call. The taps see an event only once
    /// the screen has it, so a runner that waits for its fence never sees
    /// it before the screen does (1446: the phone folds a big first pull a
    /// slice at a time and lets go of its lock between slices).
    pub fn pump_at_most(
        &mut self,
        outbox: &mut impl Outbox,
        max: usize,
    ) -> Result<Vec<OutboxEvent>, OutboxGone> {
        if let Some(commands) = &self.commands_rx {
            submit_waiting(commands, outbox)?;
        }
        let mut events = Vec::new();
        while events.len() < max {
            let Some(event) = outbox.try_next_event()? else {
                break;
            };
            self.deliver_one(&event);
            events.push(event);
        }
        Ok(events)
    }

    /// Lend the engine's end to a [`Reader`] on another thread (1492): it
    /// reads the engine and hands the taps' commands on, and the screen's
    /// side hands what it read to the taps with [`Fanout::deliver`] as it
    /// folds it. The outbox back as the error while a reader holds it
    /// already.
    pub fn lend<O: Outbox>(&mut self, outbox: O) -> Result<Reader<O>, O> {
        match self.commands_rx.take() {
            Some(commands) => Ok(Reader { outbox, commands }),
            None => Err(outbox),
        }
    }

    /// The engine's end back from its [`Reader`]: [`Fanout::pump`] hands
    /// the taps' commands on again.
    pub fn take_back<O>(&mut self, reader: Reader<O>) -> O {
        self.commands_rx = Some(reader.commands);
        reader.outbox
    }

    /// What a [`Reader`] read, copied to every live tap as
    /// [`Fanout::pump`] copies it, when the screen takes it: a tap holds
    /// no more than the screen took (1446).
    pub fn deliver(&mut self, events: &[OutboxEvent]) {
        for event in events {
            self.deliver_one(event);
        }
    }

    fn deliver_one(&mut self, event: &OutboxEvent) {
        if matches!(event, OutboxEvent::Ready { .. }) {
            self.ready = Some(event.clone());
        }
        self.taps.retain(|tap| tap.send(event.clone()).is_ok());
    }
}

/// The taps' commands waiting, handed to the engine. The fan-out holds a
/// sender itself, so the queue is never disconnected: an error is the
/// empty queue.
fn submit_waiting(commands: &Receiver<Command>, outbox: &impl Outbox) -> Result<(), OutboxGone> {
    while let Ok(command) = commands.try_recv() {
        outbox.submit(command)?;
    }
    Ok(())
}

/// The engine's end of a [`Fanout`], lent to a thread that only reads
/// (1492): the terminal reads the engine there, so an engine step that
/// stalls never stands between a key and the model.
pub struct Reader<O> {
    outbox: O,
    commands: Receiver<Command>,
}

impl<O: Outbox> Reader<O> {
    /// The taps' commands handed to the engine, then at most `max` of the
    /// engine's events, for the screen's side to fold and
    /// [`Fanout::deliver`].
    pub fn read_at_most(&mut self, max: usize) -> Result<Vec<OutboxEvent>, OutboxGone> {
        submit_waiting(&self.commands, &self.outbox)?;
        let mut events = Vec::new();
        while events.len() < max {
            let Some(event) = self.outbox.try_next_event()? else {
                break;
            };
            events.push(event);
        }
        Ok(events)
    }
}

/// A runner's outbox: the engine's events as the fan-out copied them, and
/// commands that reach the engine on the next pump.
pub struct Tap {
    commands: Sender<Command>,
    events: Receiver<OutboxEvent>,
}

impl Outbox for Tap {
    fn submit(&self, command: Command) -> Result<(), OutboxGone> {
        self.commands.send(command).map_err(|_| OutboxGone)
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(OutboxGone),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::fake::FakeOutbox;
    use std::sync::{Arc, Mutex};

    const ADDRESS: [u8; 16] = [0x42; 16];

    #[test]
    fn every_tap_and_the_screen_see_every_event() {
        let mut engine = FakeOutbox::new(vec![OutboxEvent::Queued {
            message_id: [1; 32],
        }]);
        let mut fanout = Fanout::default();
        let mut a = fanout.tap();
        let mut b = fanout.tap();
        let screen = fanout.pump(&mut engine).expect("pump");
        assert_eq!(screen.len(), 1);
        for tap in [&mut a, &mut b] {
            assert_eq!(tap.try_next_event().expect("live"), Some(screen[0].clone()));
            assert_eq!(tap.try_next_event().expect("live"), None);
        }
    }

    #[test]
    fn a_late_tap_is_handed_the_ready_the_engine_said_once() {
        let mut engine = FakeOutbox::new(vec![OutboxEvent::Ready { address: ADDRESS }]);
        let mut fanout = Fanout::default();
        fanout.pump(&mut engine).expect("pump");
        let mut late = fanout.tap();
        assert_eq!(
            late.try_next_event().expect("live"),
            Some(OutboxEvent::Ready { address: ADDRESS })
        );
    }

    #[test]
    fn a_taps_commands_reach_the_engine_on_the_next_pump() {
        let mut engine = FakeOutbox::new(Vec::new());
        let mut fanout = Fanout::default();
        let tap = fanout.tap();
        tap.submit(Command::Resolve {
            destination: ADDRESS,
        })
        .expect("live");
        assert!(engine.commands.borrow().is_empty());
        fanout.pump(&mut engine).expect("pump");
        assert_eq!(
            *engine.commands.borrow(),
            [Command::Resolve {
                destination: ADDRESS
            }]
        );
    }

    /// 1446: a slice takes at most its count; the rest stay in the
    /// engine's queue, and a tap holds only what the screen took, so a
    /// fence the runner waits for reaches it no earlier than the screen.
    #[test]
    fn a_slice_takes_at_most_its_count_and_a_tap_no_more_than_the_screen() {
        let queued = |n: u8| OutboxEvent::Queued {
            message_id: [n; 32],
        };
        let mut engine = FakeOutbox::new((0..5).map(queued).collect());
        let mut fanout = Fanout::default();
        let mut tap = fanout.tap();
        let first = fanout.pump_at_most(&mut engine, 2).expect("pump");
        assert_eq!(first, [queued(0), queued(1)]);
        let mut seen = Vec::new();
        while let Some(event) = tap.try_next_event().expect("live") {
            seen.push(event);
        }
        assert_eq!(seen, first, "the tap holds what the screen took, no more");
        let rest = fanout.pump_at_most(&mut engine, 0).expect("pump");
        assert!(rest.is_empty(), "a slice of none takes none");
        let rest = fanout.pump(&mut engine).expect("pump");
        assert_eq!(rest, [queued(2), queued(3), queued(4)]);
    }

    /// 1464: a slice folds no longer than its budget and one chunk past
    /// it, says a backlog while events wait, and its rounds fold them all
    /// in order; `stop` ends it after the chunk in hand.
    ///
    /// The slices run on the test's clock, which each fold moves on by its
    /// cost (1623): before, a fold slept its millisecond on the wall, and
    /// under a parallel run on a host at load 28 one round read 47 ms
    /// against the 20 ms budget and 15 ms of slack. The bound is now exact,
    /// the budget and one chunk.
    #[test]
    fn a_slice_ends_at_its_budget_and_its_rounds_fold_everything() {
        let queued = |n: u16| OutboxEvent::Queued {
            message_id: {
                let mut id = [0u8; 32];
                id[..2].copy_from_slice(&n.to_be_bytes());
                id
            },
        };
        let mut engine = FakeOutbox::new((0..400).map(queued).collect());
        let mut fanout = Fanout::default();
        let slice = Slice {
            chunk: 1,
            budget: Duration::from_millis(20),
        };
        let fold_cost = Duration::from_millis(1);
        let clock = Arc::new(Mutex::new(Duration::ZERO));
        let now = || *clock.lock().expect("the clock");
        let mut folded = Vec::new();
        let mut rounds = 0;
        let mut longest = Duration::ZERO;
        loop {
            rounds += 1;
            let began = now();
            let read = clock.clone();
            let mut slicing =
                Slicing::on(slice, Box::new(move || *read.lock().expect("the clock")));
            while let Some(events) = slicing.next(&mut fanout, &mut engine, false).expect("pump") {
                *clock.lock().expect("the clock") += fold_cost * events.len() as u32;
                folded.extend(events);
            }
            longest = longest.max(now() - began);
            if !slicing.backlog() {
                break;
            }
        }
        assert_eq!(folded, (0..400).map(queued).collect::<Vec<_>>());
        assert!(rounds >= 10, "400 ms of folding in {rounds} rounds");
        // At most one chunk past the budget.
        assert!(
            longest <= slice.budget + fold_cost,
            "a round took {longest:?}"
        );

        // `stop` ends the slice after the chunk in hand: the waiting
        // screen's turn comes after one event.
        let mut engine = FakeOutbox::new((0..10).map(queued).collect());
        let mut taken = 0;
        let mut slicing = Slicing::new(slice);
        while let Some(events) = slicing.next(&mut fanout, &mut engine, true).expect("pump") {
            taken += events.len();
        }
        assert!(slicing.backlog());
        assert_eq!(taken, 1);

        // The control: one pump folds everything at once.
        let mut engine = FakeOutbox::new((0..10).map(queued).collect());
        assert_eq!(fanout.pump(&mut engine).expect("pump").len(), 10);
    }

    #[test]
    fn a_finished_runners_tap_is_dropped() {
        let mut engine = FakeOutbox::new(vec![
            OutboxEvent::Ready { address: ADDRESS },
            OutboxEvent::Ready { address: ADDRESS },
        ]);
        let mut fanout = Fanout::default();
        drop(fanout.tap());
        fanout.pump(&mut engine).expect("pump");
        assert!(fanout.taps.is_empty());
    }

    /// 1492: a lent reader takes the engine's events and hands the taps'
    /// commands on; a tap holds an event only once the screen's side
    /// delivered it, and the fan-out pumps again once it has it back.
    #[test]
    fn a_lent_reader_reads_and_a_tap_sees_only_what_was_delivered() {
        let queued = |n: u8| OutboxEvent::Queued {
            message_id: [n; 32],
        };
        let engine = FakeOutbox::new((0..3).map(queued).collect());
        let mut fanout = Fanout::default();
        let mut tap = fanout.tap();
        tap.submit(Command::Resolve {
            destination: ADDRESS,
        })
        .expect("live");
        let Ok(mut reader) = fanout.lend(engine) else {
            panic!("the first lend")
        };
        assert!(
            fanout.lend(FakeOutbox::new(Vec::new())).is_err(),
            "lent once"
        );
        let read = reader.read_at_most(2).expect("read");
        assert_eq!(read, [queued(0), queued(1)]);
        assert_eq!(
            *reader.outbox.commands.borrow(),
            [Command::Resolve {
                destination: ADDRESS
            }]
        );
        assert_eq!(tap.try_next_event().expect("live"), None, "not delivered");
        fanout.deliver(&read);
        assert_eq!(tap.try_next_event().expect("live"), Some(queued(0)));
        assert_eq!(tap.try_next_event().expect("live"), Some(queued(1)));
        let mut engine = fanout.take_back(reader);
        tap.submit(Command::Resolve {
            destination: ADDRESS,
        })
        .expect("live");
        assert_eq!(fanout.pump(&mut engine).expect("pump"), [queued(2)]);
        assert_eq!(
            engine.commands.borrow().len(),
            2,
            "commands handed on again"
        );
        assert_eq!(tap.try_next_event().expect("live"), Some(queued(2)));
    }
}
