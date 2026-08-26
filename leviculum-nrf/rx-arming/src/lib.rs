#![no_std]
//! The order in which the LoRa receive path arms the radio and hands a frame
//! upward, the decision of whether it has to arm at all, and the state that
//! says whether the chip is listening.
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
//! # Why the ordering fix alone bought nothing
//!
//! It re-armed earlier and delivered no more packets. The sweep that measured
//! it also explains it: the provisional window the re-arm opens is torn down
//! again by the loop's very next decision, because `SetRx` leaves no window
//! standing behind it — an implementation that can be called while already
//! armed puts the chip in standby first. A `SetStandby` issued during a
//! frame's airtime ends that reception, so a frame that started 20 ms behind
//! its predecessor was still on the air when the teardown arrived, and a frame
//! that started 60 ms behind found the software already parked in the wait.
//! **The 50-60 ms threshold is not a property of the chip or the medium; it is
//! the interval at which the loop interrupts itself.**
//!
//! [`ensure_armed`] is the other half of the fix. Arming becomes *ensure a
//! window with these parameters is standing*: a window standing with the same
//! parameters is adopted and nothing at all is issued to the chip, a window
//! standing with different parameters is stood down as before because the loop
//! genuinely wants a different one, and no window standing arms as before.
//!
//! # The edge that has already passed
//!
//! On an adopted window the terminating IRQ may have fired while nobody was
//! waiting — that is the entire point of adopting. Waiting for an edge that
//! has already passed hangs the receive path until the next unrelated event,
//! which on a bench looks exactly like a quiet channel. [`await_window`]
//! therefore asks the chip what it has already latched *before* it waits on
//! anything, and takes a completed reception straight out of it;
//! `a_latched_reception_is_taken_without_waiting_for_the_edge` is the test
//! that pins it, and `an_unlatched_window_waits_for_the_edge` is the control
//! that shows the harness can go red.
//!
//! # What this crate deliberately does not do
//!
//! No spacing, no jitter, no delay, and no continuous RX. The re-arm uses the
//! window that just fired, and it is provisional: the loop's next decision
//! either adopts it, replaces it, or leaves RX, and leaving RX owes exactly
//! one standby. [`RxArmState`] is what tracks that debt.
//!
//! # What the instrument is, and is not
//!
//! Neither line adds a guard or a deferral. [`stand_down`] takes the same
//! standby the caller already took, in the same place, and reads back what the
//! chip latched first; [`ensure_armed`] reads the same status on the window it
//! adopts and issues nothing.
//!
//! Every `[SX_RX_ADOPT]` carrying a latched preamble, header or `RxDone` is a
//! frame the previous code destroyed — the counterfactual, measured rather
//! than argued, and measurable only because the fix counts what it saves.
//! Every `[SX_RX_TEARDOWN]` carrying one is a frame this still loses. Both
//! lines are emitted with the flags as read, including the all-zero case: a
//! counter that only speaks when it has bad news gives a numerator with no
//! denominator, and the question is a rate.

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
    ///
    /// `PartialEq` because it is the whole of the adoption decision: two
    /// windows are the same window iff every parameter they were programmed
    /// from is the same. See [`ensure_armed`].
    type Window: Copy + PartialEq;
    /// What a reception carries besides its bytes (RSSI and SNR on the SX1262).
    type Meta;
    /// Whatever the port fails with. The driver's is `sx1262::Error`.
    type Error;

    /// Issue `SetRx`: the chip starts listening.
    ///
    /// Unconditional, and it must leave no window standing behind it — an
    /// implementation that can be called while already armed puts the chip in
    /// standby first, so the chip is never armed twice.
    ///
    /// **Not the arming decision.** Whether a `SetRx` is wanted at all is
    /// [`ensure_armed`]'s question, and this is what it calls when the answer
    /// is yes. A caller that reaches for this directly re-introduces exactly
    /// the teardown the batch removed.
    async fn arm(&mut self, window: Self::Window) -> Result<(), Self::Error>;

    /// Wait for the standing window's terminating IRQ and read the frame out
    /// of the chip's buffer. Returns the number of bytes written into `buf`.
    ///
    /// The chip has left RX when this returns, by its own transition on
    /// `RxDone`/`Timeout` or by a forced standby.
    ///
    /// May block on an edge, so it must only be reached with nothing latched
    /// yet — [`await_window`] is what guarantees that.
    async fn await_frame(&mut self, buf: &mut [u8]) -> Result<(u8, Self::Meta), Self::Error>;

    /// Put the chip in standby, with no instrument attached. A no-op when no
    /// window is standing, so every path that leaves RX may call it
    /// unconditionally and still spend exactly one standby.
    ///
    /// Callers in the firmware go through [`stand_down`] instead, which is
    /// this plus the status read that says what the window was holding.
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
/// The sequence is `ensure_armed` → `await_window` → `ensure_armed` →
/// `deliver`, and the second arming is not conditional on anything: there is
/// no path from the readout to `deliver` that does not pass through it. That
/// is the guarantee, and `arming_precedes_the_hand_off` is the test that holds
/// it.
///
/// The re-arm uses the same window that just fired. The window this cycle will
/// want next is not knowable here — it depends on what the loop decides after
/// the frame reaches the core — so the honest provisional choice is to keep
/// listening exactly as we were. What the loop's next decision then does with
/// it is [`ensure_armed`]'s business: if it wants the same window it adopts
/// this one rather than replacing it, and the frame already on the air
/// survives.
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
    R: RxWindowProbe,
    S: FrameSink<Meta = R::Meta>,
{
    ensure_armed(radio, window).await?;
    let (len, meta) = await_window(radio, buf).await?;
    // The radio is listening again from here. Everything below — the buffer
    // slice, the sink's logging and reassembly, the channel send that wakes
    // the main task and yields the CPU to it — happens with the receiver
    // live. Before this line existed, all of it happened in standby.
    let rearm = ensure_armed(radio, window).await.map(|_| ());
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
/// Since [`ensure_armed`], it carries a second invariant that the first one
/// implies but nobody had to rely on before: **a window recorded here is a
/// window the chip may still be listening on.** Adoption issues nothing to
/// the chip, so a state that said "armed" about a chip sitting in standby
/// would park the receive path on an edge that can never come. The two
/// transitions that end a window — [`chip_left_rx`](Self::chip_left_rx) on
/// `RxDone`/`Timeout` and [`disarmed`](Self::disarmed) after a standby we
/// issued — are therefore recorded before anything can ask.
///
/// **Pessimistic on purpose.** [`arming`](Self::arming) is called *before*
/// `SetRx` goes out, not after, because the firmware's idle branch runs the
/// whole receive cycle inside a `select` and drops that future the instant the
/// daemon has something to send. A future dropped inside the arming SPI
/// transaction leaves a chip that may or may not be listening; recording the
/// intent first means the drop still owes a standby, and one standby too many
/// costs a command while one too few drives `SetTx` from RX.
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
    /// half programmed and for the adoption decision that compares it.
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

/// What the chip had latched while a window was standing.
///
/// Three bits and the word they came out of. `PreambleDetected` says something
/// was on the air, `HeaderValid` says it was a frame for this modulation and
/// its length was already known, `RxDone` says the reception completed while
/// nobody was waiting on it. A window with none of them was listening to an
/// empty channel and costs nothing to stand down; a window with any of them
/// holds a reception that a standby destroys.
///
/// Decoded by the port rather than derived here so the bit values stay in the
/// one crate that owns them — this crate has no dependency on the chip's
/// register map and gains nothing from one. `raw` rides along for the capture:
/// it is the only field that can show a bit nobody thought to decode, and
/// `CrcErr` on an adopted window is exactly such a bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RxLatch {
    /// The status word as the port read it, rendered as-is.
    pub raw: u16,
    /// `PreambleDetected` was latched during the window.
    pub preamble: bool,
    /// `HeaderValid` was latched during the window.
    pub header: bool,
    /// `RxDone` was latched during the window: a whole frame is sitting in the
    /// chip's buffer.
    pub rxdone: bool,
}

impl RxLatch {
    /// Nothing latched: the window was standing on an empty channel.
    pub const CLEAR: Self = Self {
        raw: 0,
        preamble: false,
        header: false,
        rxdone: false,
    };

    /// Whether this window was holding a reception. The discriminator the two
    /// instrument lines exist to feed: a teardown of a window with this false
    /// is how half duplex works, one with it true is a lost frame.
    pub const fn caught_something(&self) -> bool {
        self.preamble || self.header || self.rxdone
    }
}

impl core::fmt::Display for RxLatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "preamble={} header={} rxdone={}",
            u8::from(self.preamble),
            u8::from(self.header),
            u8::from(self.rxdone)
        )
    }
}

/// One standing window adopted by an arming that wanted exactly it.
///
/// [`Display`](core::fmt::Display) is the body of the `[SX_RX_ADOPT]` line, so
/// the shape the host greps is pinned by a test rather than by a
/// `format_args!` in a crate that has no test target — the same arrangement
/// `leviculum_core::sx126x::RxArm` uses for `[SX_RX_ARM]`.
///
/// There is no `site=`: an adoption issues nothing, so the window is the one
/// the last `[SX_RX_ARM]` named, and the parameters had to match for the
/// adoption to happen at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxAdopt {
    /// What the chip had latched at the moment of adoption. Anything set here
    /// is a reception the pre-adoption code would have destroyed.
    pub latch: RxLatch,
    /// How long the adopted window had been standing, in milliseconds,
    /// measured from its own arming rather than from the loop iteration that
    /// adopted it.
    pub stood_ms: u32,
}

impl core::fmt::Display for RxAdopt {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "latched={:#06x} {} stood_ms={}",
            self.latch.raw, self.latch, self.stood_ms
        )
    }
}

/// One standing window stood down.
///
/// [`Display`](core::fmt::Display) is the body of the `[SX_RX_TEARDOWN]` line.
///
/// `site` names the **caller** — which of the loop's paths stood the window
/// down — and not the window's own tag. That is the field the batch that
/// introduced this line got wrong by exclusion: a re-arm was assumed not to
/// be a lost reception, "the same window continuing", and the sweep refuted
/// it. Which window was standing is recoverable from the capture anyway; it
/// is whatever the last `[SX_RX_ARM]` said. Which path took it down is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxTeardown {
    /// The path that issued the standby: an arming that wanted different
    /// parameters, a key-up, a reconfigure.
    pub site: &'static str,
    /// What the chip had latched when the teardown read it.
    pub latch: RxLatch,
    /// How long the window had been standing, in milliseconds, measured from
    /// its own arming. A preamble that latched 5 ms in and one that latched
    /// 400 ms in are different stories, and without this they are one number.
    pub armed_ms: u32,
}

impl core::fmt::Display for RxTeardown {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "site={} {} armed_ms={}",
            self.site, self.latch, self.armed_ms
        )
    }
}

/// What the instrument saw, handed to the port for logging.
///
/// The sequence decides *when* an event happens and lives here, where a fake
/// radio can assert it; the port decides *how* it is emitted, because the log
/// macro, the tag and the timestamp are the firmware's. Neither half can be
/// written without the other going through this enum.
pub enum RxEvent<'a, E> {
    /// A standing window was adopted: nothing was issued to the chip.
    Adopted(&'a RxAdopt),
    /// A standing window was stood down.
    TornDown(&'a RxTeardown),
    /// The status read failed, so this sample is lost — the window itself is
    /// unaffected. Reported rather than swallowed: a rate computed from a
    /// population with invisible holes is wrong in the direction that says
    /// "no problem here". `at` is the site that would have been on the line.
    ProbeFailed {
        /// Which sequence lost the sample: [`ADOPT_SITE`], or the teardown's
        /// own caller tag.
        at: &'static str,
        /// What the port failed with.
        error: &'a E,
    },
}

/// The `at=` an adoption's failed status read reports.
pub const ADOPT_SITE: &str = "adopt";

/// A standing window, as the adoption decision and the teardown instrument
/// need to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandingWindow<W> {
    /// What `SetRx` was programmed from. Compared, never re-issued.
    pub window: W,
    /// How long it has been standing, in milliseconds, measured from its own
    /// arming.
    pub stood_ms: u32,
}

/// What a port must be able to answer about the window that is standing.
///
/// Separate from [`RxPort`] on purpose: that trait is the three transitions
/// the chip has and nothing else, and these are observations plus one
/// side-channel for the log. Neither observation may move the chip —
/// `standing_window` touches no bus at all, and `latched` is a status read.
#[allow(async_fn_in_trait)]
pub trait RxWindowProbe: RxPort {
    /// The standing window and how long it has stood. `None` when none is —
    /// the same question [`RxArmState::standby_owed`] answers, asked of the
    /// port that owns the state.
    fn standing_window(&self) -> Option<StandingWindow<Self::Window>>;

    /// Read back the interrupts the chip has latched during the standing
    /// window.
    ///
    /// **Must not clear them.** The window may be about to be stood down and
    /// the arming that follows clears the status itself; on an adopted window
    /// a clear here would consume the terminating IRQ that the awaiting half
    /// is about to take.
    async fn latched(&mut self) -> Result<RxLatch, Self::Error>;

    /// Take a reception the chip has already completed, without waiting on any
    /// edge. `Ok(None)` when no terminating IRQ is latched yet, in which case
    /// nothing has been consumed and the caller may wait.
    ///
    /// This is the hazard of adoption made into a method: on a window that was
    /// adopted rather than armed, the terminating IRQ may have fired while
    /// nobody was waiting, and an implementation that goes straight to the
    /// edge would hang until an unrelated one arrives.
    async fn take_latched_frame(
        &mut self,
        buf: &mut [u8],
    ) -> Result<Option<(u8, Self::Meta)>, Self::Error>;

    /// Emit one instrument event. Called once per adoption and once per
    /// teardown of a standing window, with the flags as read.
    fn report(&mut self, event: RxEvent<'_, Self::Error>);
}

/// What [`ensure_armed`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arming {
    /// A window with these parameters was already standing and was kept.
    /// Nothing was issued to the chip.
    Adopted,
    /// `SetRx` was issued. Any window that was standing was stood down first.
    Armed,
}

/// Ensure a window with these parameters is standing.
///
/// The batch's whole change, in three cases:
///
/// - a window standing with **the same** parameters is adopted — no standby,
///   no `SetRx`, nothing issued to the chip at all, and whatever that window
///   is holding survives;
/// - a window standing with **different** parameters is stood down and
///   replaced, because the loop genuinely wants a different one;
/// - no window standing arms.
///
/// "Same parameters" is `Window`'s own `PartialEq` and nothing softer: the
/// driver's window is the `SetRx` duration paired with the site tag, so two
/// windows are the same window only if the chip would be programmed
/// identically *and* the capture would call it the same thing. The idle path
/// is the common case and it matches exactly — the provisional re-arm uses the
/// window that just fired, and the loop's next idle decision asks for the same
/// one.
///
/// **A window that has stood too long is still adopted, and that is not an
/// oversight.** For the idle window (single mode, no hardware timeout) there
/// is no such thing as too long: it listens until a frame arrives. For a
/// bounded window, adoption shortens the listen by the time already served,
/// which is exactly what `stood_ms` on the line reports; and one that has
/// already outrun its hardware timeout has `Timeout` latched, which
/// [`await_window`] takes immediately rather than waiting for. So the worst
/// case is a window that ends earlier than the caller asked and a loop that
/// comes round again — against a teardown that ends a frame mid-air.
pub async fn ensure_armed<R>(radio: &mut R, window: R::Window) -> Result<Arming, R::Error>
where
    R: RxWindowProbe,
{
    match radio.standing_window() {
        Some(standing) if standing.window == window => {
            // Read before anything else, and never cleared: on an adopted
            // window this status IS the reception, and the awaiting half is
            // about to take it out of the same register.
            match radio.latched().await {
                Ok(latch) => {
                    let adopt = RxAdopt {
                        latch,
                        stood_ms: standing.stood_ms,
                    };
                    radio.report(RxEvent::Adopted(&adopt));
                }
                // A lost sample, not a lost window. Tearing the window down
                // because the instrument failed would be the instrument
                // causing the defect it measures.
                Err(e) => radio.report(RxEvent::ProbeFailed {
                    at: ADOPT_SITE,
                    error: &e,
                }),
            }
            Ok(Arming::Adopted)
        }
        // Different parameters, or nothing standing: `arm` is the unconditional
        // `SetRx`, and its own head stands down whatever was there — which is
        // where that teardown gets counted.
        _ => {
            radio.arm(window).await?;
            Ok(Arming::Armed)
        }
    }
}

/// Take the standing window's reception, waiting on the edge only if the chip
/// has not already latched one.
///
/// The status read comes first unconditionally rather than only on an adopted
/// window. Whether the window was armed or adopted is not something this has
/// to know, and a decision that depends on it is a decision that goes wrong
/// the first time a fourth path opens a window: a freshly armed window has a
/// cleared status and the read costs one short transaction and returns
/// `None`.
pub async fn await_window<R>(radio: &mut R, buf: &mut [u8]) -> Result<(u8, R::Meta), R::Error>
where
    R: RxWindowProbe,
{
    if let Some(frame) = radio.take_latched_frame(buf).await? {
        return Ok(frame);
    }
    radio.await_frame(buf).await
}

/// Stand a listening receiver down, and say what was on the air when it went
/// down.
///
/// Instrument only. The sequence is unchanged from before it existed except
/// for the status read: exactly one standby is spent, at the same point, and
/// the read happens strictly before it so the chip's latched flags still
/// describe the window rather than the standby that ended it.
///
/// When nothing is standing this is the same no-op [`RxPort::disarm`] always
/// was, and it emits nothing: there was no window to destroy, so there is no
/// sample.
///
/// `site` is the caller's own tag, not the window's — see [`RxTeardown`]. One
/// implementation for every path that leaves RX, rather than one for the
/// key-ups and another for the re-arms: two of those drift, and the drift is
/// what cost the last batch its measurement.
pub async fn stand_down<R>(radio: &mut R, site: &'static str) -> Result<(), R::Error>
where
    R: RxWindowProbe,
{
    let Some(standing) = radio.standing_window() else {
        return radio.disarm().await;
    };
    // Before the standby, and before any `?`: the standby is owed whatever
    // this read did, and a chip left listening while the next command is
    // `SetTx` is the one state the arming discipline exists to prevent.
    match radio.latched().await {
        Ok(latch) => {
            let teardown = RxTeardown {
                site,
                latch,
                armed_ms: standing.stood_ms,
            };
            radio.report(RxEvent::TornDown(&teardown));
        }
        Err(e) => radio.report(RxEvent::ProbeFailed {
            at: site,
            error: &e,
        }),
    }
    radio.disarm().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll};
    extern crate alloc;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// Drive a future to completion on the host.
    ///
    /// A noop waker and a re-poll loop: the fakes below never wait on anything
    /// external, they only need to be able to return `Pending` once so a
    /// blocking hand-off can be modelled. A future that parks forever — the
    /// fake's DIO1 wait with no edge coming — must not be given to this; that
    /// is what [`poll_bounded`] is for.
    fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        let mut cx = Context::from_waker(core::task::Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    /// Poll a future a bounded number of times. `None` means it was still
    /// pending — for these fakes, that it parked on the DIO1 edge.
    fn poll_bounded<F: Future>(f: F) -> Option<F::Output> {
        let mut f = pin!(f);
        let mut cx = Context::from_waker(core::task::Waker::noop());
        for _ in 0..64 {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return Some(v);
            }
        }
        None
    }

    /// Every operation the fake radio and the fake sink perform.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Op {
        Arm(u32),
        /// The DIO1 wait was entered. On an adopted window whose reception is
        /// already latched, this must never appear.
        Await,
        /// The pre-wait status read, and whether it found a reception.
        TryTake(bool),
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
        /// The instrument's status read. Recorded so "before the standby" and
        /// "exactly once" are assertions rather than descriptions.
        ProbeIrq,
        /// A rendered `[SX_RX_ADOPT]` body.
        Adopt(String),
        /// A rendered `[SX_RX_TEARDOWN]` body.
        Teardown(String),
        /// A lost sample, with the site that would have been on the line.
        ProbeErr(String),
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
        /// Every rendered body of this kind, in the order they were emitted.
        fn lines(&self, f: impl Fn(&Op) -> Option<String>) -> Vec<String> {
            self.0.borrow().iter().filter_map(|op| f(op)).collect()
        }
        /// The rendered body of the one line of this kind, or a panic naming
        /// what was logged instead.
        fn one_line(&self, f: impl Fn(&Op) -> Option<String>) -> String {
            let found = self.lines(f);
            assert_eq!(found.len(), 1, "ops={:?}", self.ops());
            found[0].clone()
        }
    }

    /// The driver's window: what `SetRx` is programmed from, paired with the
    /// site tag the capture reads. Same shape as `Sx1262`'s, so "same
    /// parameters" means the same thing here as on the board.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Window {
        timeout_ms: u32,
        site: &'static str,
    }

    const IDLE: Window = Window {
        timeout_ms: 0,
        site: "idle",
    };
    const CSMA: Window = Window {
        timeout_ms: 120,
        site: "csma",
    };

    /// A fake SX1262. Owns the same [`RxArmState`] the driver owns and takes
    /// the same decisions from it, so a departure from RX costs a standby here
    /// exactly when it costs one on the board.
    struct FakePort<'a> {
        log: &'a OpLog,
        state: RxArmState<Window>,
        /// Frames the chip will produce, one per completed reception.
        inbox: Vec<Option<Vec<u8>>>,
        /// Fail the Nth `arm` call (0-based).
        fail_arm_at: Option<usize>,
        arms: usize,
        /// Fake monotonic clock in milliseconds, advanced by the test. The
        /// driver's is `embassy_time::Instant::now`.
        now: u32,
        /// Reading of `now` taken when the standing window was armed. This
        /// is what makes `armed_ms`/`stood_ms` a measurement rather than a
        /// constant: a test that advances the clock across a whole receive
        /// cycle can tell "since the arming" from "since the loop iteration".
        armed_at: u32,
        /// What the fake chip has latched, as `latched` reports it.
        latch: RxLatch,
        /// Whether a DIO1 edge is still to come. False models the hazard: the
        /// terminating IRQ fired while nobody was waiting, so a wait on the
        /// edge parks forever.
        edge_to_come: bool,
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
                latch: RxLatch::CLEAR,
                edge_to_come: true,
                fail_probe: false,
            }
        }

        /// Pop one frame out of the fake chip's buffer.
        fn readout(&mut self, buf: &mut [u8]) -> Result<(u8, i16), ()> {
            self.state.chip_left_rx();
            self.latch = RxLatch::CLEAR;
            match self.inbox.pop() {
                Some(Some(frame)) => {
                    let n = frame.len().min(buf.len());
                    buf[..n].copy_from_slice(&frame[..n]);
                    Ok((n as u8, -42))
                }
                _ => Err(()),
            }
        }

        /// The driver's `transmit()`/`cad()` head: leave RX, then key.
        ///
        /// Both go through [`stand_down`], exactly as the driver's do, so the
        /// standby-accounting controls below run against the path the firmware
        /// takes.
        async fn transmit(&mut self) -> Result<(), ()> {
            let outcome = stand_down(self, "tx").await;
            self.log.push(Op::Transmit);
            outcome
        }

        async fn cad(&mut self) -> Result<(), ()> {
            let outcome = stand_down(self, "cad").await;
            self.log.push(Op::Cad);
            outcome
        }
    }

    impl RxWindowProbe for FakePort<'_> {
        fn standing_window(&self) -> Option<StandingWindow<Window>> {
            self.state.window().map(|w| StandingWindow {
                window: *w,
                stood_ms: self.now.saturating_sub(self.armed_at),
            })
        }

        async fn latched(&mut self) -> Result<RxLatch, ()> {
            self.log.push(Op::ProbeIrq);
            if self.fail_probe {
                return Err(());
            }
            Ok(self.latch)
        }

        async fn take_latched_frame(&mut self, buf: &mut [u8]) -> Result<Option<(u8, i16)>, ()> {
            let ready = self.latch.rxdone;
            self.log.push(Op::TryTake(ready));
            if !ready {
                return Ok(None);
            }
            self.readout(buf).map(Some)
        }

        fn report(&mut self, event: RxEvent<'_, ()>) {
            let op = match event {
                RxEvent::Adopted(a) => Op::Adopt(format!("{a}")),
                RxEvent::TornDown(t) => Op::Teardown(format!("{t}")),
                RxEvent::ProbeFailed { at, .. } => Op::ProbeErr(format!("at={at}")),
            };
            self.log.push(op);
        }
    }

    impl RxPort for FakePort<'_> {
        type Window = Window;
        type Meta = i16;
        type Error = ();

        async fn arm(&mut self, window: Window) -> Result<(), ()> {
            // Same head as the driver's `arm_rx`: never armed twice, and the
            // teardown that costs is counted where it happens.
            stand_down(self, "arm").await?;
            let n = self.arms;
            self.arms += 1;
            // Pessimistic, exactly as the driver records it — and the arm
            // instant is stamped with it, so a window that never reached
            // `SetRx` still has an honest start.
            self.armed_at = self.now;
            self.latch = RxLatch::CLEAR;
            self.state.arming(window);
            if self.fail_arm_at == Some(n) {
                return Err(());
            }
            self.log.push(Op::Arm(window.timeout_ms));
            Ok(())
        }

        async fn await_frame(&mut self, buf: &mut [u8]) -> Result<(u8, i16), ()> {
            self.log.push(Op::Await);
            if !self.edge_to_come {
                // The edge has already passed and nobody was waiting: this is
                // the hang the hazard is about, modelled rather than described.
                Never.await;
            }
            self.readout(buf)
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

    /// Never completes and never wakes: the fake's DIO1 edge, once it has
    /// passed.
    struct Never;
    impl Future for Never {
        type Output = ();
        fn poll(self: core::pin::Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
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
        block_on(receive_and_hand_up(port, &mut buf, IDLE, &mut sink))
    }

    // The ordering half, unchanged by this batch and re-asserted through the
    // adopting arm.

    /// The first batch's whole claim: the second `SetRx` is issued before
    /// anything is done with the frame, including the hand-off that yields the
    /// CPU.
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
            .filter(|op| !matches!(op, Op::DisarmNoop | Op::TryTake(_)))
            .collect();
        assert_eq!(
            significant,
            alloc::vec![
                &Op::Arm(0),
                &Op::Await,
                &Op::Arm(0),
                &Op::Deliver(alloc::vec![9, 9]),
            ],
            "ops={ops:?}"
        );
    }

    // The adoption half: this batch.

    /// The change itself. A window standing with the parameters the caller
    /// wants is kept: no standby, no `SetRx`, nothing issued to the chip.
    #[test]
    fn an_adopted_window_issues_nothing_to_the_chip() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(port.arm(IDLE)).expect("first arm");
        let before = log.ops().len();

        let arming = block_on(ensure_armed(&mut port, IDLE)).expect("adopt");

        assert_eq!(arming, Arming::Adopted);
        let after = &log.ops()[before..];
        assert_eq!(
            after
                .iter()
                .filter(|op| matches!(op, Op::Arm(_) | Op::Disarm))
                .count(),
            0,
            "an adoption issues nothing to the chip, after={after:?}"
        );
        assert!(port.state.standby_owed(), "the window is still standing");
    }

    /// A window the caller does not want is replaced — and the standby that
    /// costs is spent exactly once, where the count can see it.
    #[test]
    fn a_different_window_stands_the_old_one_down_exactly_once() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(port.arm(IDLE)).expect("first arm");
        let before = log.ops().len();

        let arming = block_on(ensure_armed(&mut port, CSMA)).expect("re-arm");

        assert_eq!(arming, Arming::Armed);
        let after = &log.ops()[before..];
        assert_eq!(
            after.iter().filter(|op| **op == Op::Disarm).count(),
            1,
            "exactly one standby, after={after:?}"
        );
        assert_eq!(
            after.iter().filter(|op| matches!(op, Op::Arm(_))).count(),
            1,
            "after={after:?}"
        );
        assert_eq!(
            port.standing_window().expect("standing").window,
            CSMA,
            "the window the loop asked for is the one standing"
        );
    }

    /// The teardown at the arming's own head is counted, and the line names
    /// the caller that took the window down.
    ///
    /// This is the site the previous batch excluded on the reasoning that a
    /// re-arm is "the same window continuing". It is not: the head issues a
    /// real standby, and a standby during a frame's airtime ends it.
    #[test]
    fn a_replaced_window_reports_what_it_was_holding() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(port.arm(IDLE)).expect("first arm");
        port.latch = RxLatch {
            raw: 0x0014,
            preamble: true,
            header: true,
            rxdone: false,
        };
        port.now += 214;

        block_on(ensure_armed(&mut port, CSMA)).expect("re-arm");

        let line = log.one_line(|op| match op {
            Op::Teardown(s) => Some(s.clone()),
            _ => None,
        });
        assert_eq!(line, "site=arm preamble=1 header=1 rxdone=0 armed_ms=214");
    }

    /// The hazard. On an adopted window the terminating IRQ may have fired
    /// while nobody was waiting; the reception is taken out of the latched
    /// status and the DIO1 wait is never entered.
    ///
    /// `edge_to_come = false` is what makes this a test rather than a
    /// description: the fake's `await_frame` parks forever, exactly as the
    /// firmware would on an edge that has already passed.
    #[test]
    fn a_latched_reception_is_taken_without_waiting_for_the_edge() {
        let log = OpLog::default();
        let payload = alloc::vec![0xAB, 0xCD];
        let mut port = FakePort::new(&log, alloc::vec![Some(payload.clone())]);
        block_on(port.arm(IDLE)).expect("arm");
        // The frame arrived and completed while the main task was running.
        port.latch = RxLatch {
            raw: 0x0002,
            preamble: true,
            header: true,
            rxdone: true,
        };
        port.edge_to_come = false;
        port.now += 20;

        block_on(ensure_armed(&mut port, IDLE)).expect("adopt");
        let mut buf = [0u8; 8];
        let got = poll_bounded(await_window(&mut port, &mut buf))
            .expect("the adopted window must not block on an edge that has passed")
            .expect("the frame must be taken from the latched status");

        assert_eq!(&buf[..got.0 as usize], &payload[..]);
        assert_eq!(
            log.count(|op| *op == Op::Await),
            0,
            "the DIO1 wait must never be entered, ops={:?}",
            log.ops()
        );
        assert!(
            !port.state.standby_owed(),
            "the chip left RX at the reception it had already completed"
        );
    }

    /// Control for the hazard test: with nothing latched the wait IS entered,
    /// and a fake whose edge has passed parks there. Without this, the test
    /// above would pass against a harness that can never block at all.
    #[test]
    fn an_unlatched_window_waits_for_the_edge() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![1])]);
        block_on(port.arm(IDLE)).expect("arm");
        port.edge_to_come = false;

        let mut buf = [0u8; 8];
        assert!(
            poll_bounded(await_window(&mut port, &mut buf)).is_none(),
            "an empty latch must reach the edge, ops={:?}",
            log.ops()
        );
        assert_eq!(log.count(|op| *op == Op::Await), 1, "ops={:?}", log.ops());
        assert_eq!(
            log.count(|op| *op == Op::TryTake(false)),
            1,
            "the status is read before the wait either way, ops={:?}",
            log.ops()
        );
    }

    /// Control: a reception reaches the sink exactly once with the same bytes,
    /// whether the window it arrived on was armed or adopted.
    #[test]
    fn a_reception_reaches_the_sink_exactly_once_adopted_and_unadopted() {
        for adopted in [false, true] {
            let log = OpLog::default();
            let payload = alloc::vec![0xDE, 0xAD, 0xBE, 0xEF];
            let mut port = FakePort::new(&log, alloc::vec![Some(payload.clone())]);
            if adopted {
                // The loop's next decision finds the provisional window
                // standing with a completed reception in it.
                block_on(port.arm(IDLE)).expect("arm");
                port.latch = RxLatch {
                    raw: 0x0002,
                    preamble: true,
                    header: true,
                    rxdone: true,
                };
                port.edge_to_come = false;
            }
            let got = run_cycle(&log, &mut port, 1).expect("fake reception");

            assert_eq!(
                log.count(|op| matches!(op, Op::Deliver(_))),
                1,
                "adopted={adopted} ops={:?}",
                log.ops()
            );
            assert_eq!(
                log.count(|op| *op == Op::Deliver(payload.clone())),
                1,
                "the delivered bytes must be the received bytes, adopted={adopted} ops={:?}",
                log.ops()
            );
            assert_eq!(got.len as usize, payload.len());
        }
    }

    /// Control: every path that leaves RX still spends exactly one standby,
    /// and an adoption in front of it changes nothing about that.
    #[test]
    fn leaving_rx_for_tx_or_cad_costs_exactly_one_standby_each() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![4])]);
        run_cycle(&log, &mut port, 1).expect("fake reception");
        // A standing (provisional) window is what the loop now finds.
        assert!(port.state.standby_owed());
        // The loop's next decision adopts it, then the daemon has data.
        block_on(ensure_armed(&mut port, IDLE)).expect("adopt");
        let before = log.ops().len();

        block_on(port.cad()).expect("cad");
        block_on(port.transmit()).expect("transmit");
        // Back to listening for the post-TX ack window.
        block_on(ensure_armed(&mut port, CSMA)).expect("re-arm");

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
            let mut fut = pin!(receive_and_hand_up(&mut port, &mut buf, IDLE, &mut sink));
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
        block_on(port.transmit()).expect("transmit");
        assert_eq!(log.count(|op| *op == Op::Disarm), 1, "ops={:?}", log.ops());
    }

    #[test]
    fn arming_state_tracks_the_three_transitions() {
        let mut state: RxArmState<Window> = RxArmState::new();
        assert!(!state.standby_owed());
        assert_eq!(state.window(), None);

        state.arming(IDLE);
        assert!(state.standby_owed());
        assert_eq!(state.window(), Some(&IDLE));

        // The chip ended the window itself: no standby is owed.
        state.chip_left_rx();
        assert!(!state.standby_owed());
        assert_eq!(state.window(), None);

        state.arming(CSMA);
        state.disarmed();
        assert!(!state.standby_owed());
    }

    /// Re-arming over a standing window replaces it rather than stacking:
    /// the awaiting half reads the window it will actually be woken by.
    #[test]
    fn arming_over_a_standing_window_replaces_it() {
        let mut state: RxArmState<Window> = RxArmState::new();
        state.arming(IDLE);
        state.arming(CSMA);
        assert_eq!(state.window(), Some(&CSMA));
        state.disarmed();
        assert!(!state.standby_owed());
    }

    // The instrument.

    /// Both lines carry the flags as read, and they carry them as the grammar
    /// the host greps.
    #[test]
    fn both_lines_render_the_flags_the_chip_latched() {
        let adopt = RxAdopt {
            latch: RxLatch {
                raw: 0x0016,
                preamble: true,
                header: true,
                rxdone: true,
            },
            stood_ms: 47,
        };
        assert_eq!(
            format!("{adopt}"),
            "latched=0x0016 preamble=1 header=1 rxdone=1 stood_ms=47"
        );
        let teardown = RxTeardown {
            site: "tx",
            latch: RxLatch {
                raw: 0x0004,
                preamble: true,
                header: false,
                rxdone: false,
            },
            armed_ms: 5,
        };
        assert_eq!(
            format!("{teardown}"),
            "site=tx preamble=1 header=0 rxdone=0 armed_ms=5"
        );
    }

    /// Control: a clean window still produces a line, on both paths. The
    /// question is a rate, and a discriminator that speaks only when it has
    /// bad news gives a numerator with no denominator.
    #[test]
    fn a_clean_window_still_reports_a_line_on_both_paths() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(port.arm(IDLE)).expect("arm");
        port.now += 312;
        block_on(ensure_armed(&mut port, IDLE)).expect("adopt");
        assert_eq!(
            log.one_line(|op| match op {
                Op::Adopt(s) => Some(s.clone()),
                _ => None,
            }),
            "latched=0x0000 preamble=0 header=0 rxdone=0 stood_ms=312"
        );

        block_on(port.transmit()).expect("transmit");
        assert_eq!(
            log.one_line(|op| match op {
                Op::Teardown(s) => Some(s.clone()),
                _ => None,
            }),
            "site=tx preamble=0 header=0 rxdone=0 armed_ms=312"
        );
    }

    /// The teardown names the caller, which is the field a capture cannot
    /// reconstruct: the window's own tag is whatever the last `[SX_RX_ARM]`
    /// said, the path that took it down is nowhere else.
    #[test]
    fn a_teardown_names_the_caller_that_took_the_window_down() {
        for (call, site) in [("cad", "cad"), ("tx", "tx")] {
            let log = OpLog::default();
            let mut port = FakePort::new(&log, Vec::new());
            block_on(port.arm(CSMA)).expect("arm");
            if call == "cad" {
                block_on(port.cad()).expect("cad");
            } else {
                block_on(port.transmit()).expect("transmit");
            }
            let line = log.one_line(|op| match op {
                Op::Teardown(s) => Some(s.clone()),
                _ => None,
            });
            assert!(
                line.starts_with(&format!("site={site} ")),
                "line={line} call={call}"
            );
        }
    }

    /// Control: the teardown spends exactly one standby, and the status read
    /// happens strictly before it — after the standby the flags describe the
    /// command that ended the window rather than the window.
    #[test]
    fn a_teardown_reads_before_the_standby_and_spends_exactly_one() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(port.arm(IDLE)).expect("arm");
        let before = log.ops().len();
        block_on(stand_down(&mut port, "select")).expect("stood down");
        assert!(!port.state.standby_owed());

        let ops = log.ops();
        let after = &ops[before..];
        assert_eq!(
            after.iter().filter(|op| **op == Op::Disarm).count(),
            1,
            "exactly one standby, after={after:?}"
        );
        assert_eq!(
            after.iter().filter(|op| **op == Op::ProbeIrq).count(),
            1,
            "exactly one status read, after={after:?}"
        );
        let probe = after
            .iter()
            .position(|op| *op == Op::ProbeIrq)
            .expect("probe");
        let standby = after
            .iter()
            .position(|op| *op == Op::Disarm)
            .expect("standby");
        assert!(probe < standby, "after={after:?}");
    }

    /// Control: no window standing is not a teardown. The disarm is the same
    /// no-op it always was, nothing is read, and no sample is invented.
    #[test]
    fn standing_down_an_unarmed_radio_is_not_a_sample() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(stand_down(&mut port, "tx")).expect("stood down");
        let ops = log.ops();
        assert_eq!(log.count(|op| *op == Op::ProbeIrq), 0, "ops={ops:?}");
        assert_eq!(
            log.count(|op| matches!(op, Op::Teardown(_))),
            0,
            "ops={ops:?}"
        );
        assert_eq!(log.count(|op| *op == Op::Disarm), 0, "ops={ops:?}");
        assert_eq!(log.count(|op| *op == Op::DisarmNoop), 1, "ops={ops:?}");
    }

    /// Control: a failed status read still stands the receiver down, and still
    /// says the sample was lost. An instrument that can block a transmit is a
    /// guard, and this batch ships no guard.
    #[test]
    fn a_failed_probe_does_not_cost_the_standby() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(port.arm(IDLE)).expect("arm");
        port.fail_probe = true;
        block_on(stand_down(&mut port, "tx")).expect("the standby is unaffected");

        assert_eq!(
            log.count(|op| *op == Op::ProbeErr(format!("at=tx"))),
            1,
            "the lost sample is reported, ops={:?}",
            log.ops()
        );
        assert!(!port.state.standby_owed());
        assert_eq!(log.count(|op| *op == Op::Disarm), 1, "ops={:?}", log.ops());
    }

    /// Control: a failed status read on an adoption keeps the window. The
    /// instrument must not become the cause of the teardown it measures.
    #[test]
    fn a_failed_probe_does_not_cost_the_adopted_window() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, Vec::new());
        block_on(port.arm(IDLE)).expect("arm");
        port.fail_probe = true;
        let before = log.ops().len();

        let arming = block_on(ensure_armed(&mut port, IDLE)).expect("adopt");

        assert_eq!(arming, Arming::Adopted);
        assert!(port.state.standby_owed(), "the window is still standing");
        let after = &log.ops()[before..];
        assert_eq!(
            after
                .iter()
                .filter(|op| matches!(op, Op::Arm(_) | Op::Disarm))
                .count(),
            0,
            "nothing was issued to the chip, after={after:?}"
        );
        assert_eq!(
            after
                .iter()
                .filter(|op| **op == Op::ProbeErr(format!("at={ADOPT_SITE}")))
                .count(),
            1,
            "after={after:?}"
        );
    }

    /// `stood_ms` and `armed_ms` are measured from the arming of the window
    /// they describe, not from the loop iteration that adopted or ended it.
    ///
    /// The clock runs across a whole receive cycle: the first window arms at
    /// t=0 and the provisional re-arm happens at t=1000, so a line that
    /// reported "since the iteration started" would say 1030.
    #[test]
    fn the_ages_are_measured_from_the_arming() {
        let log = OpLog::default();
        let mut port = FakePort::new(&log, alloc::vec![Some(alloc::vec![1])]);
        block_on(port.arm(IDLE)).expect("first arm");
        // The window stands for a second before the frame lands.
        port.now += 1_000;
        // The reception's provisional re-arm: a new window, a new start.
        block_on(port.arm(IDLE)).expect("re-arm");
        port.now += 30;

        block_on(ensure_armed(&mut port, IDLE)).expect("adopt");
        assert!(
            log.one_line(|op| match op {
                Op::Adopt(s) => Some(s.clone()),
                _ => None,
            })
            .ends_with("stood_ms=30"),
            "measured from the re-arm at t=1000, not from t=0, ops={:?}",
            log.ops()
        );

        port.now += 7;
        block_on(port.transmit()).expect("transmit");
        let teardowns = log.lines(|op| match op {
            Op::Teardown(s) => Some(s.clone()),
            _ => None,
        });
        assert!(
            teardowns[0].ends_with("armed_ms=1000"),
            "the re-arm's head reports the window it replaced, at its own age, \
             ops={:?}",
            log.ops()
        );
        assert!(
            teardowns[1].ends_with("armed_ms=37"),
            "the adoption did not restart the clock, ops={:?}",
            log.ops()
        );
    }

    /// The discriminator itself: a window is a lost frame only if it was
    /// holding one.
    #[test]
    fn a_latch_says_whether_the_window_was_holding_a_reception() {
        assert!(!RxLatch::CLEAR.caught_something());
        for latch in [
            RxLatch {
                raw: 0x0004,
                preamble: true,
                header: false,
                rxdone: false,
            },
            RxLatch {
                raw: 0x0010,
                preamble: false,
                header: true,
                rxdone: false,
            },
            RxLatch {
                raw: 0x0002,
                preamble: false,
                header: false,
                rxdone: true,
            },
        ] {
            assert!(latch.caught_something(), "latch={latch:?}");
        }
    }
}
