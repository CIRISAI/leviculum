//! Outbound BLE notification flow control (Codeberg #264).
//!
//! A Reticulum packet larger than one BLE fragment is handed to the
//! SoftDevice one `sd_ble_gatts_hvx` call at a time, and the SoftDevice
//! accepts at most `hvn_tx_queue_size` notifications per connection
//! before it starts refusing with `NRF_ERROR_RESOURCES`. On S140 that
//! queue is **one** entry deep by default
//! (`BLE_GATTS_HVN_TX_QUEUE_SIZE_DEFAULT = 1`), so from the second
//! fragment onwards *every* call is refused until the first one has
//! actually gone out over the air. A loop that ignores the return value
//! therefore delivers fragment 0 and silently discards 1..N — the peer
//! waits forever for an assembly that will never complete, and nothing
//! on our side knows a packet was lost.
//!
//! The decision that fixes this — retry the same fragment after the
//! queue drains, abort and report on anything else, never re-send a
//! fragment that already went out — is a state machine over notify
//! results and SoftDevice events. Nothing in it needs a radio, so it
//! lives here and is exercised on the host, next to
//! [`leviculum-sd-policy`] and [`leviculum-telemetry-policy`] and for
//! the same reason: a state machine that needs hardware to be exercised
//! is a state machine that is never exercised.
//!
//! The firmware side stays a thin driver: perform the [`Action`] the
//! machine asks for, feed back the [`Event`] it produced, repeat. Each
//! action is performed **exactly once**, which is what makes
//! "every fragment goes out in order and exactly once" a structural
//! property rather than a hope — the machine is the only source of
//! actions, and it only ever emits [`Action::Send`] for a fragment it
//! has not yet seen acknowledged.
//!
//! [`leviculum-sd-policy`]: https://codeberg.org/Lew_Palm/leviculum
//! [`leviculum-telemetry-policy`]: https://codeberg.org/Lew_Palm/leviculum

#![cfg_attr(not(test), no_std)]

/// Upper bound on a single wait for the SoftDevice's
/// `BLE_GATTS_EVT_HVN_TX_COMPLETE`, in milliseconds.
///
/// The queue drains at most one notification per connection event, i.e.
/// once per connection interval. Centrals that matter for a Reticulum
/// link — Columba on Android, BlueZ, iOS — negotiate intervals in the
/// 7.5 ms to 100 ms range while the link is active, and our own
/// `conn_gap.event_length` of 24 (× 1.25 ms = 30 ms) is sized for that
/// regime. 2 s is therefore at least 20 connection events even at the
/// slow end of it: a healthy link never reaches this bound.
///
/// The upper side is bounded by the link-supervision timeout. Once the
/// peer stops responding the stack tears the connection down on its own
/// and the next `notify_value` returns `Disconnected`. Waiting longer
/// than a couple of seconds would just race that teardown and replace a
/// precise "the queue stalled" diagnosis with a generic disconnect. 2 s
/// sits below the multi-second supervision timeouts in common use, so a
/// stall is reported as a stall.
///
/// Residual: a spec-legal but exotic central negotiating an interval
/// above 2 s would see spurious aborts. That peer is out of scope for an
/// interactive mesh link, and the abort is *visible* (event + counter)
/// rather than silent, which is strictly better than the behaviour it
/// replaces.
pub const DRAIN_WAIT_MS: u64 = 2_000;

/// How many drain waits one packet may spend before it is abandoned.
///
/// With a one-deep queue the expected cost is one wait per fragment
/// after the first, and each wait ends with a drain event that means a
/// fragment genuinely left the device. `2 * fragments + 4` leaves room
/// for a stale drain edge left over from an earlier packet and for a
/// deeper queue, while still bounding a pathological peer that keeps
/// signalling drains without ever making room. Hitting the budget is
/// itself a diagnosis, which is why it aborts with its own reason
/// instead of looping.
#[must_use]
pub fn drain_wait_budget(fragment_count: usize) -> u32 {
    let n = u32::try_from(fragment_count).unwrap_or(u32::MAX / 4);
    n.saturating_mul(2).saturating_add(4)
}

/// The result of one `sd_ble_gatts_hvx` call, as the driver saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyOutcome {
    /// The SoftDevice took the fragment into its notification queue.
    Sent,
    /// `NRF_ERROR_RESOURCES`: the per-connection HVN queue is full. The
    /// fragment was **not** queued and must be offered again.
    QueueFull,
    /// The connection is gone; nothing more will go out on it.
    Disconnected,
    /// Any other SoftDevice error, carrying its raw code so the report
    /// names it. `NRF_ERROR_DATA_SIZE` (a fragment larger than the
    /// negotiated ATT MTU allows) is the one to expect here.
    Failed(u32),
}

/// Something that happened after the driver performed an [`Action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The outcome of the [`Action::Send`] the driver just performed.
    Notify(NotifyOutcome),
    /// The SoftDevice raised `BLE_GATTS_EVT_HVN_TX_COMPLETE`: at least
    /// one queue slot is free again.
    Drained,
    /// [`Action::AwaitDrain`] hit [`DRAIN_WAIT_MS`] without a drain.
    WaitTimedOut,
}

/// Why a packet was abandoned part-way through its fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    /// No `HVN_TX_COMPLETE` within [`DRAIN_WAIT_MS`].
    Stalled,
    /// The connection dropped mid-packet.
    Disconnected,
    /// [`drain_wait_budget`] exhausted.
    BudgetExhausted,
    /// A SoftDevice error other than `NRF_ERROR_RESOURCES`.
    SoftDevice(u32),
}

impl AbortReason {
    /// Stable token for the `reason=` field of the structured log event.
    /// Whitespace-free and `=`-free, as the event-log grammar requires.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AbortReason::Stalled => "stalled",
            AbortReason::Disconnected => "disconnected",
            AbortReason::BudgetExhausted => "budget",
            AbortReason::SoftDevice(_) => "sd_error",
        }
    }

    /// The raw SoftDevice code for the `code=` field, 0 when the reason
    /// did not come from a syscall.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            AbortReason::SoftDevice(code) => code,
            _ => 0,
        }
    }
}

/// What the driver must do next. Perform it exactly once, then feed the
/// resulting [`Event`] back through [`PacketTx::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Hand fragment `index` to `notify_value`.
    Send { index: usize },
    /// Wait for [`Event::Drained`], bounded by [`DRAIN_WAIT_MS`]; on
    /// expiry feed [`Event::WaitTimedOut`]. `index` is the fragment the
    /// wait is for, for the log line only.
    AwaitDrain { index: usize },
    /// Every fragment was queued, in order, exactly once.
    Done,
    /// Give up on this packet. `index` is the fragment that failed —
    /// fragments before it did go out, the ones from it on did not.
    Abort { index: usize, reason: AbortReason },
    /// The event did not apply to the outstanding action. The driver
    /// must not act on it; it still owes the event for the action it
    /// performed. Reachable only from a caller that feeds an event it
    /// was not asked for, and defined explicitly so that such a caller
    /// cannot provoke a duplicate `Send`.
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Fragment `.0` was handed to `notify_value`; its result is owed.
    Sending(usize),
    /// Fragment `.0` was refused; a drain (or a timeout) is owed.
    Waiting(usize),
    Done,
    Aborted,
}

/// Drives one packet's fragments through the SoftDevice notification
/// queue. Construct with [`PacketTx::start`], then loop on
/// [`PacketTx::step`].
#[derive(Debug)]
pub struct PacketTx {
    total: usize,
    phase: Phase,
    sent: usize,
    waits: u32,
    budget: u32,
}

impl PacketTx {
    /// Begin a packet of `fragment_count` fragments, returning the first
    /// action. A zero-fragment packet is [`Action::Done`] at once —
    /// there is nothing to send and nothing to report.
    #[must_use]
    pub fn start(fragment_count: usize) -> (Self, Action) {
        let (phase, action) = if fragment_count == 0 {
            (Phase::Done, Action::Done)
        } else {
            (Phase::Sending(0), Action::Send { index: 0 })
        };
        let tx = Self {
            total: fragment_count,
            phase,
            sent: 0,
            waits: 0,
            budget: drain_wait_budget(fragment_count),
        };
        (tx, action)
    }

    /// Feed the event produced by the last action; get the next action.
    pub fn step(&mut self, event: Event) -> Action {
        match (self.phase, event) {
            (Phase::Sending(index), Event::Notify(NotifyOutcome::Sent)) => {
                self.sent += 1;
                let next = index + 1;
                if next >= self.total {
                    self.phase = Phase::Done;
                    Action::Done
                } else {
                    self.phase = Phase::Sending(next);
                    Action::Send { index: next }
                }
            }
            (Phase::Sending(index), Event::Notify(NotifyOutcome::QueueFull)) => {
                if self.waits >= self.budget {
                    self.abort(index, AbortReason::BudgetExhausted)
                } else {
                    self.phase = Phase::Waiting(index);
                    Action::AwaitDrain { index }
                }
            }
            (Phase::Sending(index), Event::Notify(NotifyOutcome::Disconnected)) => {
                self.abort(index, AbortReason::Disconnected)
            }
            (Phase::Sending(index), Event::Notify(NotifyOutcome::Failed(code))) => {
                self.abort(index, AbortReason::SoftDevice(code))
            }
            (Phase::Waiting(index), Event::Drained) => {
                self.waits += 1;
                self.phase = Phase::Sending(index);
                Action::Send { index }
            }
            (Phase::Waiting(index), Event::WaitTimedOut) => self.abort(index, AbortReason::Stalled),
            _ => Action::Nothing,
        }
    }

    fn abort(&mut self, index: usize, reason: AbortReason) -> Action {
        self.phase = Phase::Aborted;
        Action::Abort { index, reason }
    }

    /// Fragments the SoftDevice accepted so far.
    #[must_use]
    pub fn fragments_sent(&self) -> usize {
        self.sent
    }

    /// Drain waits this packet has completed, for the `waits=` field.
    #[must_use]
    pub fn drain_waits(&self) -> u32 {
        self.waits
    }

    /// The packet's drain-wait budget, i.e. [`drain_wait_budget`] of its
    /// fragment count.
    #[must_use]
    pub fn budget(&self) -> u32 {
        self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NRF_ERROR_DATA_SIZE`, the error a notification larger than the
    /// negotiated ATT MTU produces. Used as the "hard error" stand-in.
    const NRF_ERROR_DATA_SIZE: u32 = 12;

    /// A scripted stand-in for the SoftDevice's per-connection HVN
    /// queue: it accepts up to `depth` notifications, refuses the rest
    /// with `QueueFull`, and only makes room when something drains it.
    struct Sink {
        depth: usize,
        in_flight: usize,
        /// Fragment indices accepted, in the order they were accepted.
        accepted: Vec<usize>,
        /// Fail the call for this fragment index instead of queueing it.
        fail_at: Option<(usize, NotifyOutcome)>,
        /// Refuse to ever drain (models a peer that stopped listening).
        never_drains: bool,
        /// Signal a drain without actually making room (models a
        /// pathological drain storm).
        lying_drains: bool,
    }

    impl Sink {
        fn new(depth: usize) -> Self {
            Self {
                depth,
                in_flight: 0,
                accepted: Vec::new(),
                fail_at: None,
                never_drains: false,
                lying_drains: false,
            }
        }

        fn notify(&mut self, index: usize) -> NotifyOutcome {
            if let Some((at, outcome)) = self.fail_at {
                if at == index {
                    return outcome;
                }
            }
            if self.in_flight >= self.depth {
                return NotifyOutcome::QueueFull;
            }
            self.in_flight += 1;
            self.accepted.push(index);
            NotifyOutcome::Sent
        }

        /// The event the driver observes while waiting for a drain.
        fn wait(&mut self) -> Event {
            if self.lying_drains {
                return Event::Drained;
            }
            if self.never_drains || self.in_flight == 0 {
                return Event::WaitTimedOut;
            }
            self.in_flight -= 1;
            Event::Drained
        }
    }

    /// The firmware driver, in the shape `ble.rs` uses it.
    fn drive(sink: &mut Sink, fragments: usize) -> (Action, PacketTx) {
        let (mut tx, mut action) = PacketTx::start(fragments);
        for _ in 0..10_000 {
            match action {
                Action::Send { index } => {
                    let outcome = sink.notify(index);
                    action = tx.step(Event::Notify(outcome));
                }
                Action::AwaitDrain { .. } => {
                    let event = sink.wait();
                    action = tx.step(event);
                }
                terminal => return (terminal, tx),
            }
        }
        panic!("driver did not terminate");
    }

    /// Positive control for the sink itself, and the red state this
    /// batch fixes: the pre-#264 loop pushed every fragment without
    /// looking at the result, so exactly one of three reached the queue.
    #[test]
    fn control_ignoring_the_result_delivers_only_the_first_fragment() {
        let mut sink = Sink::new(1);
        for index in 0..3 {
            let _ignored = sink.notify(index);
        }
        assert_eq!(sink.accepted, vec![0], "the sink must actually refuse");
    }

    #[test]
    fn every_fragment_goes_out_in_order_exactly_once_on_a_one_deep_queue() {
        let mut sink = Sink::new(1);
        let (action, tx) = drive(&mut sink, 3);
        assert_eq!(action, Action::Done);
        assert_eq!(sink.accepted, vec![0, 1, 2]);
        assert_eq!(tx.fragments_sent(), 3);
    }

    #[test]
    fn a_queue_that_refuses_after_k_fragments_then_drains_still_delivers_all() {
        // depth 2: fragments 0 and 1 go straight in, 2 and 3 each wait.
        let mut sink = Sink::new(2);
        let (action, tx) = drive(&mut sink, 4);
        assert_eq!(action, Action::Done);
        assert_eq!(sink.accepted, vec![0, 1, 2, 3]);
        assert_eq!(tx.drain_waits(), 2);
    }

    #[test]
    fn a_queue_deep_enough_never_waits() {
        let mut sink = Sink::new(8);
        let (action, tx) = drive(&mut sink, 3);
        assert_eq!(action, Action::Done);
        assert_eq!(sink.accepted, vec![0, 1, 2]);
        assert_eq!(tx.drain_waits(), 0);
    }

    #[test]
    fn success_after_n_retries_counts_one_wait_per_refused_fragment() {
        let mut sink = Sink::new(1);
        let (action, tx) = drive(&mut sink, 5);
        assert_eq!(action, Action::Done);
        assert_eq!(sink.accepted, vec![0, 1, 2, 3, 4]);
        assert_eq!(tx.drain_waits(), 4);
    }

    #[test]
    fn a_peer_that_stops_draining_aborts_at_the_stalled_fragment() {
        let mut sink = Sink::new(1);
        sink.never_drains = true;
        let (action, tx) = drive(&mut sink, 3);
        assert_eq!(
            action,
            Action::Abort {
                index: 1,
                reason: AbortReason::Stalled
            }
        );
        assert_eq!(sink.accepted, vec![0], "fragment 1 was never queued");
        assert_eq!(tx.fragments_sent(), 1);
    }

    #[test]
    fn a_hard_error_mid_packet_aborts_and_carries_the_softdevice_code() {
        let mut sink = Sink::new(8);
        sink.fail_at = Some((1, NotifyOutcome::Failed(NRF_ERROR_DATA_SIZE)));
        let (action, _tx) = drive(&mut sink, 3);
        assert_eq!(
            action,
            Action::Abort {
                index: 1,
                reason: AbortReason::SoftDevice(NRF_ERROR_DATA_SIZE)
            }
        );
        assert_eq!(sink.accepted, vec![0]);
        let Action::Abort { reason, .. } = action else {
            unreachable!()
        };
        assert_eq!(reason.as_str(), "sd_error");
        assert_eq!(reason.code(), NRF_ERROR_DATA_SIZE);
    }

    #[test]
    fn a_disconnect_mid_packet_aborts_without_a_softdevice_code() {
        let mut sink = Sink::new(8);
        sink.fail_at = Some((2, NotifyOutcome::Disconnected));
        let (action, _tx) = drive(&mut sink, 4);
        assert_eq!(
            action,
            Action::Abort {
                index: 2,
                reason: AbortReason::Disconnected
            }
        );
        assert_eq!(sink.accepted, vec![0, 1]);
        assert_eq!(AbortReason::Disconnected.as_str(), "disconnected");
        assert_eq!(AbortReason::Disconnected.code(), 0);
    }

    #[test]
    fn drain_events_that_never_make_room_exhaust_the_budget_instead_of_looping() {
        let mut sink = Sink::new(1);
        sink.lying_drains = true;
        // depth 1 with a lying drain: fragment 0 goes in, fragment 1 is
        // refused forever while the "drain" keeps firing.
        let (action, tx) = drive(&mut sink, 2);
        assert_eq!(
            action,
            Action::Abort {
                index: 1,
                reason: AbortReason::BudgetExhausted
            }
        );
        assert_eq!(tx.drain_waits(), tx.budget());
        assert_eq!(tx.budget(), drain_wait_budget(2));
    }

    #[test]
    fn an_event_the_machine_did_not_ask_for_cannot_provoke_a_second_send() {
        // Phase Sending(0): a stray drain must not re-issue Send{0}.
        let (mut tx, first) = PacketTx::start(2);
        assert_eq!(first, Action::Send { index: 0 });
        assert_eq!(tx.step(Event::Drained), Action::Nothing);
        assert_eq!(tx.step(Event::WaitTimedOut), Action::Nothing);
        // The outstanding result still advances the packet normally.
        assert_eq!(
            tx.step(Event::Notify(NotifyOutcome::Sent)),
            Action::Send { index: 1 }
        );

        // Phase Waiting(1): a stray notify result must not advance it.
        assert_eq!(
            tx.step(Event::Notify(NotifyOutcome::QueueFull)),
            Action::AwaitDrain { index: 1 }
        );
        assert_eq!(tx.step(Event::Notify(NotifyOutcome::Sent)), Action::Nothing);
        assert_eq!(tx.fragments_sent(), 1);
    }

    #[test]
    fn a_terminated_packet_stays_terminated() {
        let (mut tx, _) = PacketTx::start(1);
        assert_eq!(tx.step(Event::Notify(NotifyOutcome::Sent)), Action::Done);
        assert_eq!(tx.step(Event::Notify(NotifyOutcome::Sent)), Action::Nothing);
        assert_eq!(tx.step(Event::Drained), Action::Nothing);

        let (mut tx, _) = PacketTx::start(1);
        assert!(matches!(
            tx.step(Event::Notify(NotifyOutcome::Disconnected)),
            Action::Abort { .. }
        ));
        assert_eq!(tx.step(Event::Drained), Action::Nothing);
        assert_eq!(tx.step(Event::Notify(NotifyOutcome::Sent)), Action::Nothing);
    }

    #[test]
    fn an_empty_packet_is_done_immediately() {
        let (tx, action) = PacketTx::start(0);
        assert_eq!(action, Action::Done);
        assert_eq!(tx.fragments_sent(), 0);
    }

    #[test]
    fn the_budget_grows_with_the_fragment_count_and_never_overflows() {
        assert_eq!(drain_wait_budget(0), 4);
        assert_eq!(drain_wait_budget(1), 6);
        assert_eq!(drain_wait_budget(3), 10);
        assert!(drain_wait_budget(usize::MAX) > 0);
    }
}
