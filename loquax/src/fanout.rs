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

use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};

/// The shell's side: the taps' queues and the `Ready` to replay.
pub struct Fanout {
    taps: Vec<Sender<OutboxEvent>>,
    commands_tx: Sender<Command>,
    commands_rx: Receiver<Command>,
    ready: Option<OutboxEvent>,
}

impl Default for Fanout {
    fn default() -> Self {
        let (commands_tx, commands_rx) = channel();
        Self {
            taps: Vec::new(),
            commands_tx,
            commands_rx,
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
        // The fan-out holds a sender itself, so the queue is never
        // disconnected: an error is the empty queue.
        while let Ok(command) = self.commands_rx.try_recv() {
            outbox.submit(command)?;
        }
        let mut events = Vec::new();
        while events.len() < max {
            let Some(event) = outbox.try_next_event()? else {
                break;
            };
            if matches!(event, OutboxEvent::Ready { .. }) {
                self.ready = Some(event.clone());
            }
            self.taps.retain(|tap| tap.send(event.clone()).is_ok());
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
}
