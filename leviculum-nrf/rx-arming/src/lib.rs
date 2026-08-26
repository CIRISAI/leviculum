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
//!
//! # What the abort instrument is, and is not
//!
//! [`stand_down_for_tx`] adds no guard and no deferral: it takes the same
//! standby the TX paths already took, in the same place, and reads back what
//! the chip latched first. Standing down an idle listen is not a loss — it is
//! how half duplex works. Standing down a window whose preamble has already
//! arrived destroys a reception that would otherwise have completed. The
//! count of aborts cannot tell those apart; [`RxAbort`] can, and until it has
//! been run on the bench nobody knows which of the two the loop is doing.

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

/// What the chip latched while a window was standing.
///
/// The two bits `leviculum_core::sx126x::RX_EXTEND_INPUTS` is built from,
/// decoded. They are the whole discriminator: `PreambleDetected` says
/// something was on the air, `HeaderValid` says it was a frame for this
/// modulation and its length was already known. A window stood down with
/// neither is an idle listen and costs nothing; a window stood down with
/// either is a reception that will not complete.
///
/// Decoded by the port rather than carried as raw flags so the bit values
/// stay in the one crate that owns them — this crate has no dependency on
/// the chip's register map and gains nothing from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RxProgress {
    /// `PreambleDetected` was latched during the window.
    pub preamble: bool,
    /// `HeaderValid` was latched during the window.
    pub header: bool,
}

impl RxProgress {
    /// Neither bit: the window was standing on an empty channel.
    pub const CLEAR: Self = Self {
        preamble: false,
        header: false,
    };
}

/// One standing window stood down because the loop is about to key.
///
/// [`Display`](core::fmt::Display) is the body of the `[SX_RX_ABORT]` line,
/// so the shape the host greps is pinned by a host test rather than by a
/// `format_args!` in a crate that has no test target — the same arrangement
/// `leviculum_core::sx126x::RxArm` uses for `[SX_RX_ARM]`.
///
/// `preamble` and `header` render as `0`/`1` and never as anything else,
/// including when the window was clean. A discriminator that only speaks
/// when it has bad news gives a numerator with no denominator, and the
/// question this exists to answer is a rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxAbort {
    /// The stood-down window's own site tag — which of the loop's windows
    /// was listening, not which path did the standing down. The port reads
    /// it back off the standing window, so it is the same tag `[SX_RX_ARM]`
    /// printed when that window opened.
    pub site: &'static str,
    /// What the chip had latched when the abort read it.
    pub progress: RxProgress,
    /// How long the window had been standing, in milliseconds, measured
    /// from its own arming. A preamble that latched 5 ms in and one that
    /// latched 400 ms in are different stories, and without this they are
    /// one number.
    pub armed_ms: u32,
}

impl core::fmt::Display for RxAbort {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "site={} preamble={} header={} armed_ms={}",
            self.site,
            u8::from(self.progress.preamble),
            u8::from(self.progress.header),
            self.armed_ms
        )
    }
}

/// What a port must be able to answer at the moment a window is stood down.
///
/// Separate from [`RxPort`] on purpose: that trait is the three transitions
/// the chip has and nothing else, and these two are observations, not
/// transitions. Neither may move the chip — `standing_window` touches no
/// bus at all, and `latched_progress` is a status read.
#[allow(async_fn_in_trait)]
pub trait RxAbortProbe: RxPort {
    /// The standing window's site tag and how long it has stood, in
    /// milliseconds. `None` when no window is standing — the same question
    /// [`RxArmState::standby_owed`] answers, asked of the port that owns
    /// the state.
    fn standing_window(&self) -> Option<(&'static str, u32)>;

    /// Read back the interrupts the chip latched during the standing
    /// window.
    ///
    /// **Must not clear them.** The window is about to be stood down and
    /// the arming that follows clears the status itself; a clear here would
    /// consume a pending terminating IRQ that the caller has not yet seen.
    async fn latched_progress(&mut self) -> Result<RxProgress, Self::Error>;
}

/// The outcome of [`stand_down_for_tx`]: what the instrument saw, and what
/// the standby returned.
///
/// Two independent results rather than one. `stood_down` is exactly what a
/// bare [`RxPort::disarm`] would have returned, so a caller can propagate it
/// unchanged; a probe that fails must not turn into a transmit that does not
/// happen.
pub struct StandDown<E> {
    /// `Ok(None)` when no window was standing, `Ok(Some)` when one was and
    /// the probe read it, `Err` when the probe itself failed. The last case
    /// is a lost sample and the caller should say so out loud — a silently
    /// dropped abort deflates the rate this measures.
    pub abort: Result<Option<RxAbort>, E>,
    /// What the standby returned.
    pub stood_down: Result<(), E>,
}

/// Stand a listening receiver down because the loop is about to key, and say
/// what was on the air when it went down.
///
/// Instrument only. The sequence is unchanged from before it existed except
/// for the status read: exactly one standby is spent, at the same point, and
/// the read happens strictly before it so the chip's latched flags still
/// describe the window rather than the standby that ended it.
///
/// When nothing is standing this is the same no-op [`RxPort::disarm`] always
/// was, and it emits nothing: there was no window to destroy, so there is no
/// sample.
pub async fn stand_down_for_tx<R>(radio: &mut R) -> StandDown<R::Error>
where
    R: RxAbortProbe,
{
    let Some((site, armed_ms)) = radio.standing_window() else {
        return StandDown {
            abort: Ok(None),
            stood_down: radio.disarm().await,
        };
    };
    // Before the standby, and before any `?`: the standby is owed whatever
    // this read did, and a chip left listening while the next command is
    // `SetTx` is the one state the arming discipline exists to prevent.
    let progress = radio.latched_progress().await;
    let stood_down = radio.disarm().await;
    StandDown {
        abort: progress.map(|progress| {
            Some(RxAbort {
                site,
                progress,
                armed_ms,
            })
        }),
        stood_down,
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
        /// The abort's status read. Recorded so "before the standby" and
        /// "exactly once" are assertions rather than descriptions.
        ProbeIrq,
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
        /// Fake monotonic clock in milliseconds, advanced by the test. The
        /// driver's is `embassy_time::Instant::now`.
        now: u32,
        /// Reading of `now` taken when the standing window was armed. This
        /// is what makes `armed_ms` a measurement rather than a constant:
        /// a test that advances the clock across a whole receive cycle can
        /// tell "since the arming" from "since the loop iteration".
        armed_at: u32,
        /// The site tag `arm` records on the window it opens.
        site: &'static str,
        /// What the fake chip has latched, as `latched_progress` reports it.
        latched: RxProgress,
        /// Fail the status read, the way an SPI error would.
        fail_probe: bool,
    }

    impl<'a> FakePort<'a> {
        fn new(log: &'a OpLog, inbox: Vec<Option<Vec<u8>>>) -> Self {
            Self {
                log,
                state: RxArmState::new(),
                inbox,
                fail_arm_at: None,
                arms: 0,
                now: 0,
                armed_at: 0,
                site: "idle",
                latched: RxProgress::CLEAR,
                fail_probe: false,
            }
        }

        /// The driver's `transmit()`/`cad()` head: leave RX, then key.
        ///
        /// Both go through [`stand_down_for_tx`], exactly as the driver's do
        /// since the abort instrument landed, so the standby-accounting
        /// controls below run against the path the firmware takes.
        async fn transmit(&mut self) -> StandDown<()> {
            let outcome = stand_down_for_tx(self).await;
            self.log.push(Op::Transmit);
            outcome
        }

        async fn cad(&mut self) -> StandDown<()> {
            let outcome = stand_down_for_tx(self).await;
            self.log.push(Op::Cad);
            outcome
        }
    }

    impl RxAbortProbe for FakePort<'_> {
        fn standing_window(&self) -> Option<(&'static str, u32)> {
            self.state
                .window()
                .map(|_| (self.site, self.now.saturating_sub(self.armed_at)))
        }

        async fn latched_progress(&mut self) -> Result<RxProgress, ()> {
            self.log.push(Op::ProbeIrq);
            if self.fail_probe {
                return Err(());
            }
            Ok(self.latched)
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
            // Pessimistic, exactly as the driver records it — and the arm
            // instant is stamped with it, so a window that never reached
            // `SetRx` still has an honest start.
            self.armed_at = self.now;
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

    /// Arm a window, advance the fake clock, and stand it down for a key-up.
    fn arm_then_abort(
        log: &OpLog,
        latched: RxProgress,
        armed_for_ms: u32,
    ) -> (FakePort<'_>, StandDown<()>) {
        let mut port = FakePort::new(log, Vec::new());
        port.latched = latched;
        block_on(port.arm(500)).expect("fake arm");
        port.now += armed_for_ms;
        let outcome = block_on(stand_down_for_tx(&mut port));
        (port, outcome)
    }

    /// The instrument itself: the line carries the flags the chip had
    /// latched, and it carries them as the grammar the host greps.
    #[test]
    fn an_abort_reports_the_flags_the_chip_latched() {
        let log = OpLog::default();
        let latched = RxProgress {
            preamble: true,
            header: true,
        };
        let (_port, outcome) = arm_then_abort(&log, latched, 47);
        let abort = outcome
            .abort
            .expect("the probe read")
            .expect("a window was standing");
        assert_eq!(abort.progress, latched);
        let mut line = alloc::string::String::new();
        core::fmt::write(&mut line, format_args!("{abort}")).expect("render");
        assert_eq!(line, "site=idle preamble=1 header=1 armed_ms=47");
    }

    /// A preamble with no header — the frame arrived but the abort beat its
    /// header — is a distinct reading and must not collapse into either
    /// neighbour.
    #[test]
    fn a_preamble_without_a_header_reports_one_and_zero() {
        let log = OpLog::default();
        let (_port, outcome) = arm_then_abort(
            &log,
            RxProgress {
                preamble: true,
                header: false,
            },
            5,
        );
        let abort = outcome.abort.expect("read").expect("standing");
        let mut line = alloc::string::String::new();
        core::fmt::write(&mut line, format_args!("{abort}")).expect("render");
        assert_eq!(line, "site=idle preamble=1 header=0 armed_ms=5");
    }

    /// Control: a clean status still produces a line. The question is a
    /// rate, and a discriminator that speaks only when it has bad news
    /// gives a numerator with no denominator.
    #[test]
    fn a_clean_abort_still_reports_a_line() {
        let log = OpLog::default();
        let (_port, outcome) = arm_then_abort(&log, RxProgress::CLEAR, 312);
        let abort = outcome
            .abort
            .expect("the probe read")
            .expect("a clean window is still a sample");
        let mut line = alloc::string::String::new();
        core::fmt::write(&mut line, format_args!("{abort}")).expect("render");
        assert_eq!(line, "site=idle preamble=0 header=0 armed_ms=312");
    }

    /// Control: the abort spends exactly one standby, and the status read
    /// happens strictly before it — after the standby the flags describe
    /// the command that ended the window rather than the window.
    #[test]
    fn an_abort_reads_before_the_standby_and_spends_exactly_one() {
        let log = OpLog::default();
        let (port, outcome) = arm_then_abort(&log, RxProgress::CLEAR, 1);
        assert!(outcome.stood_down.is_ok());
        assert!(!port.state.standby_owed());

        let ops = log.ops();
        assert_eq!(
            log.count(|op| *op == Op::Disarm),
            1,
            "exactly one standby, ops={ops:?}"
        );
        assert_eq!(
            log.count(|op| *op == Op::ProbeIrq),
            1,
            "exactly one status read, ops={ops:?}"
        );
        let probe = log.position(|op| *op == Op::ProbeIrq).expect("probe");
        let standby = log.position(|op| *op == Op::Disarm).expect("standby");
        assert!(probe < standby, "ops={ops:?}");
    }

    /// Control: no window standing is not an abort. The disarm is the same
    /// no-op it always was, nothing is read, and no sample is invented.
    #[test]
    fn standing_down_an_unarmed_radio_is_not_a_sample() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        let outcome = block_on(stand_down_for_tx(&mut port));
        assert_eq!(outcome.abort, Ok(None));
        assert!(outcome.stood_down.is_ok());
        let ops = log.ops();
        assert_eq!(log.count(|op| *op == Op::ProbeIrq), 0, "ops={ops:?}");
        assert_eq!(log.count(|op| *op == Op::Disarm), 0, "ops={ops:?}");
        assert_eq!(log.count(|op| *op == Op::DisarmNoop), 1, "ops={ops:?}");
    }

    /// Control: a failed status read still stands the receiver down, and
    /// still reports the standby's own result. An instrument that can block
    /// a transmit is a guard, and this batch ships no guard.
    #[test]
    fn a_failed_probe_does_not_cost_the_standby() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        port.fail_probe = true;
        block_on(port.arm(500)).expect("fake arm");
        let outcome = block_on(stand_down_for_tx(&mut port));

        assert_eq!(outcome.abort, Err(()), "the lost sample is reported");
        assert!(
            outcome.stood_down.is_ok(),
            "the standby is unaffected, ops={:?}",
            log.ops()
        );
        assert!(!port.state.standby_owed());
        assert_eq!(log.count(|op| *op == Op::Disarm), 1, "ops={:?}", log.ops());
    }

    /// `armed_ms` is measured from the arming of the window that is being
    /// stood down, not from the loop iteration that decided to key.
    ///
    /// The clock runs across a whole receive cycle: the first window arms at
    /// t=0 and the provisional re-arm happens at t=1000, so an abort at
    /// t=1030 that reported "since the iteration started" would say 1030.
    #[test]
    fn armed_ms_is_measured_from_the_arming() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![1])]);
        block_on(port.arm(500)).expect("first arm");
        // The window stands for a second before the frame lands.
        port.now += 1_000;
        // The reception's provisional re-arm: a new window, a new start.
        block_on(port.arm(500)).expect("re-arm");
        port.now += 30;

        let outcome = block_on(stand_down_for_tx(&mut port));
        let abort = outcome.abort.expect("read").expect("standing");
        assert_eq!(
            abort.armed_ms, 30,
            "measured from the re-arm at t=1000, not from t=0"
        );
    }

    /// The site is the stood-down window's own, so a capture says which of
    /// the loop's windows the key-up destroyed — not which path keyed.
    #[test]
    fn the_abort_names_the_window_that_was_standing() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        port.site = "csma";
        block_on(port.arm(120)).expect("fake arm");
        let abort = block_on(stand_down_for_tx(&mut port))
            .abort
            .expect("read")
            .expect("standing");
        assert_eq!(abort.site, "csma");
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
