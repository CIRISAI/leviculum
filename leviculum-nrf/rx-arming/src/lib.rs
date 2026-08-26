#![no_std]
//! The order in which the LoRa receive path arms the radio and hands a frame
//! upward, and the state that says whether the chip is listening.
//!
//! # The ordering this crate exists to fix
//!
//! The receive loop used to hand a frame up first and re-arm the receiver
//! afterwards. The hand-off is `incoming_tx.send(data).await`, which wakes the
//! main task; the LoRa task's next `Poll::Pending` fell inside the re-arm
//! sequence, so the main task ran `handle_packet` to completion — announce
//! signature verification included — with the radio in standby. Measured off
//! the air: a receiver running the reference firmware takes a packet 20 ms
//! behind another, ours needed roughly 50 ms.
//!
//! [`receive_and_hand_up`] makes that ordering false by construction rather
//! than by comment: it re-arms between the buffer readout and the hand-off,
//! and the hand-off is reached through no other path. A fake port and a fake
//! sink can therefore assert the order of operations on the same function the
//! firmware runs, which the firmware crate itself cannot do — it
//! cross-compiles to `thumbv7em-none-eabihf` and has no test target.
//!
//! # What this crate deliberately does not do
//!
//! No spacing, no jitter, no delay, and no continuous RX. The re-arm uses the
//! window that just fired, and it is provisional: the loop's next decision
//! either awaits it or leaves RX, and leaving RX owes exactly one standby.
//! [`RxArmState`] is what tracks that debt.

/// The receiver operations the arming order is defined over.
///
/// One implementation, `Sx1262`, and one fake per test. The methods are the
/// three transitions the chip actually has — into RX, out of RX by itself,
/// out of RX because we said so — and nothing else, so an implementation
/// cannot satisfy the trait without exposing the transition an ordering bug
/// would hide.
///
/// `async fn` rather than the `-> impl Future` form used elsewhere in the
/// tree: two of these methods take borrows besides `&mut self`, and the
/// desugaring an `async fn` implementation produces for those cannot be
/// written by hand in a return-position bound (`impl Future + 'a + 'b` is not
/// a legal type). The lint the form raises is about missing `Send` bounds for
/// generic callers; there is exactly one caller, a single-threaded Embassy
/// task, so there is nothing for a `Send` bound to buy here.
#[allow(async_fn_in_trait)]
pub trait RxPort {
    /// How a listening window is described to the chip. The driver's is the
    /// pair `SetRx` is programmed from plus the site tag the log line carries.
    type Window: Copy;
    /// What a reception carries besides its bytes (RSSI and SNR on the SX1262).
    type Meta;
    /// Whatever the port fails with. The driver's is `sx1262::Error`.
    type Error;

    /// Issue `SetRx`: the chip starts listening.
    ///
    /// Must leave no window standing behind it — an implementation that can be
    /// called while already armed puts the chip in standby first, so the chip
    /// is never armed twice.
    async fn arm(&mut self, window: Self::Window) -> Result<(), Self::Error>;

    /// Await the standing window's terminating IRQ and read the frame out of
    /// the chip's buffer. Returns the number of bytes written into `buf`.
    ///
    /// The chip has left RX when this returns, by its own transition on
    /// `RxDone`/`Timeout` or by a forced standby.
    async fn await_frame(&mut self, buf: &mut [u8]) -> Result<(u8, Self::Meta), Self::Error>;

    /// Put the chip in standby. A no-op when no window is standing, so every
    /// path that leaves RX may call it unconditionally and still spend exactly
    /// one standby.
    async fn disarm(&mut self) -> Result<(), Self::Error>;
}

/// Where a reception goes once the radio is listening again.
///
/// Allowed to block, and in the firmware it does: the hand-off is a bounded
/// channel send that wakes the main task and yields to it. That it may block
/// is the entire reason the re-arm has to precede it.
#[allow(async_fn_in_trait)]
pub trait FrameSink {
    /// Must match the port's [`RxPort::Meta`].
    type Meta;

    /// Hand one reception upward.
    async fn deliver(&mut self, bytes: &[u8], meta: &Self::Meta);
}

/// One completed reception, after the frame has been handed up.
pub struct Reception<M, E> {
    /// Bytes the chip reported, as written into the caller's buffer.
    pub len: u8,
    /// The port's per-reception metadata.
    pub meta: M,
    /// Outcome of the re-arm that ran before the hand-off.
    ///
    /// Carried rather than propagated: a good reception is not dropped because
    /// the SPI transaction that re-opened the window failed. The caller logs
    /// it; the next window arms from scratch anyway, because [`RxArmState`]
    /// keeps the standby owed.
    pub rearm: Result<(), E>,
}

/// Run one receive window and hand what it caught upward — with the radio
/// listening again before the hand-off starts.
///
/// The sequence is `arm` → `await_frame` → `arm` → `deliver`, and the second
/// `arm` is not conditional on anything: there is no path from `await_frame`
/// to `deliver` that does not pass through it. That is the guarantee, and
/// `arming_precedes_the_hand_off` is the test that holds it.
///
/// The re-arm uses the same window that just fired. The window this cycle will
/// want next is not knowable here — it depends on what the loop decides after
/// the frame reaches the core — so the honest provisional choice is to keep
/// listening exactly as we were, and let the next decision replace it.
///
/// A window that ends without a frame (timeout, CRC error, SPI error) does
/// **not** re-arm: the re-arm exists to cover the hand-off, and there is no
/// hand-off to cover. The chip is in standby and the loop's next window arms
/// it, exactly as before this function existed.
pub async fn receive_and_hand_up<R, S>(
    radio: &mut R,
    buf: &mut [u8],
    window: R::Window,
    sink: &mut S,
) -> Result<Reception<R::Meta, R::Error>, R::Error>
where
    R: RxPort,
    S: FrameSink<Meta = R::Meta>,
{
    radio.arm(window).await?;
    let (len, meta) = radio.await_frame(buf).await?;
    // The radio is listening again from here. Everything below — the buffer
    // slice, the sink's logging and reassembly, the channel send that wakes
    // the main task and yields the CPU to it — happens with the receiver
    // live. Before this line existed, all of it happened in standby.
    let rearm = radio.arm(window).await;
    let n = (len as usize).min(buf.len());
    sink.deliver(&buf[..n], &meta).await;
    Ok(Reception { len, meta, rearm })
}

/// Whether the receiver is listening, and with what.
///
/// The invariant it exists to keep is "the chip must not end up armed twice,
/// or armed while transmitting": every path that leaves RX asks
/// [`standby_owed`](Self::standby_owed) and spends one standby if it says so.
///
/// **Pessimistic on purpose.** [`arming`](Self::arming) is called *before*
/// `SetRx` goes out, not after, because the firmware's idle branch runs the
/// whole receive cycle inside a `select` and drops that future the instant the
/// daemon has something to send. A future dropped inside the arming SPI
/// transaction leaves a chip that may or may not be listening; recording the
/// intent first means the drop still owes a standby, and one standby too many
/// costs a command while one too few drives `SetTx` from RX.
///
/// The two ways a window ends are kept apart in the names because they differ
/// in what the caller must do, not in what this type records:
/// [`chip_left_rx`](Self::chip_left_rx) after `RxDone`/`Timeout`, where the
/// chip returned to STBY_RC on its own and a standby would be wasted, and
/// [`disarmed`](Self::disarmed) after a standby we issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxArmState<W> {
    armed: Option<W>,
}

impl<W> RxArmState<W> {
    /// No window standing: nothing owes a standby.
    pub const fn new() -> Self {
        Self { armed: None }
    }

    /// Whether a standby has to be spent before the chip may do anything but
    /// receive. True from just before `SetRx` until the window is known to
    /// have ended.
    pub const fn standby_owed(&self) -> bool {
        self.armed.is_some()
    }

    /// The standing window, for the awaiting half that needs what the arming
    /// half programmed.
    pub fn window(&self) -> Option<&W> {
        self.armed.as_ref()
    }

    /// Record that `SetRx` is about to be issued for `window`.
    pub fn arming(&mut self, window: W) {
        self.armed = Some(window);
    }

    /// Record that the chip ended the window itself (`RxDone` or the hardware
    /// timeout, both of which return it to STBY_RC).
    pub fn chip_left_rx(&mut self) {
        self.armed = None;
    }

    /// Record that a standby we issued has completed.
    pub fn disarmed(&mut self) {
        self.armed = None;
    }
}

impl<W> Default for RxArmState<W> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll};
    extern crate alloc;
    use alloc::vec::Vec;

    /// Drive a future to completion on the host.
    ///
    /// A noop waker and a re-poll loop: the fakes below never wait on anything
    /// external, they only need to be able to return `Pending` once so a
    /// blocking hand-off can be modelled.
    fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        let mut cx = Context::from_waker(core::task::Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    /// Every operation the fake radio and the fake sink perform.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Op {
        Arm(u32),
        Await,
        Disarm,
        /// A standby the port skipped because no window was standing. Recorded
        /// so "exactly once" can be told apart from "at least once".
        DisarmNoop,
        Deliver(Vec<u8>),
        /// The hand-off yielded — the point at which, in the firmware, the main
        /// task runs.
        HandOffPending,
        Transmit,
        Cad,
    }

    /// One log for the radio and the hand-off together.
    ///
    /// Shared, not two logs concatenated afterwards. The first version of this
    /// harness gave the port and the sink a vec each and appended one to the
    /// other at the end, which put every sink op last no matter when it ran —
    /// so the ordering assertions passed against a deliberately inverted
    /// `receive_and_hand_up`. A harness that cannot go red is not a control.
    #[derive(Default)]
    struct OpLog(RefCell<Vec<Op>>);

    impl OpLog {
        fn push(&self, op: Op) {
            self.0.borrow_mut().push(op);
        }
        fn ops(&self) -> Vec<Op> {
            self.0.borrow().clone()
        }
        fn count(&self, f: impl Fn(&Op) -> bool) -> usize {
            self.0.borrow().iter().filter(|op| f(op)).count()
        }
        fn position(&self, f: impl Fn(&Op) -> bool) -> Option<usize> {
            self.0.borrow().iter().position(|op| f(op))
        }
    }

    /// A fake SX1262. Owns the same [`RxArmState`] the driver owns and takes
    /// the same decisions from it, so a departure from RX costs a standby here
    /// exactly when it costs one on the board.
    struct FakePort<'a> {
        log: &'a OpLog,
        state: RxArmState<u32>,
        /// Frames the chip will produce, one per `await_frame`.
        inbox: Vec<Option<Vec<u8>>>,
        /// Fail the Nth `arm` call (0-based).
        fail_arm_at: Option<usize>,
        arms: usize,
    }

    impl<'a> FakePort<'a> {
        fn new(log: &'a OpLog, inbox: Vec<Option<Vec<u8>>>) -> Self {
            Self {
                log,
                state: RxArmState::new(),
                inbox,
                fail_arm_at: None,
                arms: 0,
            }
        }

        /// The driver's `transmit()`/`cad()` head: leave RX, then key.
        async fn transmit(&mut self) {
            self.disarm().await.expect("fake disarm cannot fail");
            self.log.push(Op::Transmit);
        }

        async fn cad(&mut self) {
            self.disarm().await.expect("fake disarm cannot fail");
            self.log.push(Op::Cad);
        }
    }

    impl RxPort for FakePort<'_> {
        type Window = u32;
        type Meta = i16;
        type Error = ();

        async fn arm(&mut self, window: u32) -> Result<(), ()> {
            // Same head as the driver's `arm_rx`: never armed twice.
            self.disarm().await?;
            let n = self.arms;
            self.arms += 1;
            // Pessimistic, exactly as the driver records it.
            self.state.arming(window);
            if self.fail_arm_at == Some(n) {
                return Err(());
            }
            self.log.push(Op::Arm(window));
            Ok(())
        }

        async fn await_frame(&mut self, buf: &mut [u8]) -> Result<(u8, i16), ()> {
            self.log.push(Op::Await);
            // Whatever the outcome, the chip has stopped listening.
            self.state.chip_left_rx();
            match self.inbox.pop() {
                Some(Some(frame)) => {
                    let n = frame.len().min(buf.len());
                    buf[..n].copy_from_slice(&frame[..n]);
                    Ok((n as u8, -42))
                }
                _ => Err(()),
            }
        }

        async fn disarm(&mut self) -> Result<(), ()> {
            if !self.state.standby_owed() {
                self.log.push(Op::DisarmNoop);
                return Ok(());
            }
            self.log.push(Op::Disarm);
            self.state.disarmed();
            Ok(())
        }
    }

    /// A fake hand-off that yields before completing, the way the firmware's
    /// channel send wakes the main task and gives it the CPU.
    struct FakeSink<'a> {
        log: &'a OpLog,
        pends: usize,
    }

    impl FrameSink for FakeSink<'_> {
        type Meta = i16;

        async fn deliver(&mut self, bytes: &[u8], _meta: &i16) {
            for _ in 0..self.pends {
                self.log.push(Op::HandOffPending);
                YieldOnce::new().await;
            }
            self.log.push(Op::Deliver(bytes.to_vec()));
        }
    }

    /// Returns `Pending` exactly once, then `Ready`.
    struct YieldOnce(bool);
    impl YieldOnce {
        fn new() -> Self {
            Self(false)
        }
    }
    impl Future for YieldOnce {
        type Output = ();
        fn poll(mut self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    fn run_cycle(
        log: &OpLog,
        port: &mut FakePort<'_>,
        pends: usize,
    ) -> Result<Reception<i16, ()>, ()> {
        let mut buf = [0u8; 8];
        let mut sink = FakeSink { log, pends };
        block_on(receive_and_hand_up(port, &mut buf, 500, &mut sink))
    }

    /// The batch's whole claim: the second `SetRx` is issued before anything
    /// is done with the frame, including the hand-off that yields the CPU.
    #[test]
    fn arming_precedes_the_hand_off() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![1, 2, 3])]);
        run_cycle(&log, &mut port, 1).expect("fake reception");

        let ops = log.ops();
        let arms: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter(|(_, op)| matches!(op, Op::Arm(_)))
            .map(|(i, _)| i)
            .collect();
        let handoff = log
            .position(|op| matches!(op, Op::HandOffPending))
            .expect("the hand-off must have yielded");
        let deliver = log
            .position(|op| matches!(op, Op::Deliver(_)))
            .expect("the frame must have been handed up");

        assert_eq!(arms.len(), 2, "ops={ops:?}");
        assert!(
            arms[1] < handoff && arms[1] < deliver,
            "the re-arm must precede the hand-off, ops={ops:?}"
        );
    }

    /// The same claim stated as the full sequence, so a reordering that keeps
    /// the two arms but moves the readout is caught too.
    #[test]
    fn the_cycle_runs_arm_await_arm_deliver() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![9, 9])]);
        run_cycle(&log, &mut port, 0).expect("fake reception");
        let ops = log.ops();
        let significant: Vec<&Op> = ops
            .iter()
            .filter(|op| !matches!(op, Op::DisarmNoop))
            .collect();
        assert_eq!(
            significant,
            alloc::vec![
                &Op::Arm(500),
                &Op::Await,
                &Op::Arm(500),
                &Op::Deliver(alloc::vec![9, 9]),
            ],
            "ops={ops:?}"
        );
    }

    /// Control: an ordering change that dropped or duplicated frames would
    /// pass the ordering test. One reception, one delivery, same bytes.
    #[test]
    fn a_reception_reaches_the_sink_exactly_once_with_the_same_bytes() {
        let log = OpLog::default();
        let payload = alloc::vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut port = FakePort::new(&log, alloc::vec![Some(payload.clone())]);
        let got = run_cycle(&log, &mut port, 1).expect("fake reception");

        assert_eq!(
            log.count(|op| matches!(op, Op::Deliver(_))),
            1,
            "ops={:?}",
            log.ops()
        );
        assert_eq!(
            log.count(|op| *op == Op::Deliver(payload.clone())),
            1,
            "the delivered bytes must be the received bytes, ops={:?}",
            log.ops()
        );
        assert_eq!(got.len as usize, payload.len());
        assert_eq!(log.count(|op| *op == Op::Await), 1, "ops={:?}", log.ops());
    }

    /// A window that catches nothing does not re-arm: the re-arm covers a
    /// hand-off, and there is none. One `SetRx`, and the chip is left in
    /// standby for the loop's next decision.
    #[test]
    fn an_empty_window_arms_once_and_hands_up_nothing() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        assert!(run_cycle(&log, &mut port, 0).is_err());
        assert_eq!(
            log.count(|op| matches!(op, Op::Arm(_))),
            1,
            "ops={:?}",
            log.ops()
        );
        assert_eq!(log.count(|op| matches!(op, Op::Deliver(_))), 0);
        assert!(!port.state.standby_owed());
    }

    /// A failed re-arm still hands the frame up. Dropping a good reception
    /// because the SPI transaction that re-opened the window failed would
    /// trade the defect this batch fixes for a worse one.
    #[test]
    fn a_failed_rearm_does_not_cost_the_frame() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![7])]);
        port.fail_arm_at = Some(1);
        let got = run_cycle(&log, &mut port, 1).expect("the reception must survive");
        assert!(got.rearm.is_err());
        assert_eq!(
            log.count(|op| matches!(op, Op::Deliver(_))),
            1,
            "ops={:?}",
            log.ops()
        );
    }

    /// Control for the deliberate departures: the standing window costs
    /// exactly one standby, whichever path leaves RX first, and the window
    /// that follows arms exactly once.
    #[test]
    fn leaving_rx_for_tx_or_cad_costs_exactly_one_standby_each() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![4])]);
        run_cycle(&log, &mut port, 1).expect("fake reception");
        // A standing (provisional) window is what the loop now finds.
        assert!(port.state.standby_owed());
        let before = log.ops().len();

        block_on(port.cad());
        block_on(port.transmit());
        // Back to listening for the post-TX ack window.
        block_on(port.arm(120)).expect("re-arm");

        let ops = log.ops();
        let after = &ops[before..];
        assert_eq!(
            after.iter().filter(|op| **op == Op::Disarm).count(),
            1,
            "the CAD takes the one standby the standing window owed, after={after:?}"
        );
        assert_eq!(
            after.iter().filter(|op| **op == Op::DisarmNoop).count(),
            2,
            "the transmit and the re-arm find nothing to stand down, after={after:?}"
        );
        assert_eq!(
            after.iter().filter(|op| matches!(op, Op::Arm(_))).count(),
            1,
            "after={after:?}"
        );
        // Neither key-up happened with a window standing.
        let cad = after.iter().position(|op| *op == Op::Cad).expect("cad");
        let tx = after
            .iter()
            .position(|op| *op == Op::Transmit)
            .expect("transmit");
        let standby = after
            .iter()
            .position(|op| *op == Op::Disarm)
            .expect("standby");
        assert!(standby < cad && standby < tx, "after={after:?}");
    }

    /// The dropped-future case the idle `select` produces: the cycle is
    /// abandoned mid-hand-off, and the standby is still owed afterwards, so
    /// the TX path that follows the drop stands the receiver down.
    #[test]
    fn a_dropped_cycle_still_owes_a_standby() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![1])]);
        let mut buf = [0u8; 8];
        {
            let mut sink = FakeSink {
                log: &log,
                pends: 1,
            };
            let mut fut = pin!(receive_and_hand_up(&mut port, &mut buf, 500, &mut sink));
            let mut cx = Context::from_waker(core::task::Waker::noop());
            // Poll until the hand-off yields, then drop the future — exactly
            // what `select` does when the daemon has outgoing data.
            assert!(fut.as_mut().poll(&mut cx).is_pending());
        }
        assert!(
            port.state.standby_owed(),
            "the provisional window is still standing, ops={:?}",
            log.ops()
        );
        block_on(port.transmit());
        assert_eq!(log.count(|op| *op == Op::Disarm), 1, "ops={:?}", log.ops());
    }

    #[test]
    fn arming_state_tracks_the_three_transitions() {
        let mut state: RxArmState<u32> = RxArmState::new();
        assert!(!state.standby_owed());
        assert_eq!(state.window(), None);

        state.arming(500);
        assert!(state.standby_owed());
        assert_eq!(state.window(), Some(&500));

        // The chip ended the window itself: no standby is owed.
        state.chip_left_rx();
        assert!(!state.standby_owed());
        assert_eq!(state.window(), None);

        state.arming(0);
        state.disarmed();
        assert!(!state.standby_owed());
    }

    /// Re-arming over a standing window replaces it rather than stacking:
    /// the awaiting half reads the window it will actually be woken by.
    #[test]
    fn arming_over_a_standing_window_replaces_it() {
        let mut state: RxArmState<u32> = RxArmState::new();
        state.arming(500);
        state.arming(120);
        assert_eq!(state.window(), Some(&120));
        state.disarmed();
        assert!(!state.standby_owed());
    }
}
