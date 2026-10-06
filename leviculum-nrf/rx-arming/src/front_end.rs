//! Which way the antenna switch is pointed, tracked beside the arm state.
//!
//! # Why this is a state and not a comment
//!
//! Every board sets `SetDIO2AsRfSwitch`, so the SX1262 raises DIO2 for the
//! duration of a transmit and the transmit side of the switch follows it. On
//! the T114 and the RAK4631 that is the whole switch. The Wio-SX1262 on the
//! solar node steers its receive side from a host GPIO as well, and a switch
//! asked for both positions at once passes neither: a transmit that goes
//! nowhere. "Off the receive path before the PA keys" used to be a comment
//! above the key-up in `sx1262.rs`, and "asserted for receive, released for
//! transmit" a claim about a helper with four call sites. Codeberg #411, the
//! solar node receiving nothing, had three candidate causes in exactly that
//! code and no way to tell them apart short of flashing.
//!
//! [`FrontEnd`] owns the RX-enable line, so the driver can no longer write it
//! except through a transition, and every transition is checked against the
//! one invariant there is: **never both paths asked for at once**
//! ([`FrontEnd::consistent`]). The two transitions that could break it are
//! refused rather than performed — arming the receiver while the PA is keyed,
//! and keying the PA while the receive path is asserted — and
//! [`FrontEnd::key_up`] is the only way to the PA, releasing the receive path
//! first.
//!
//! # The same rules on a board without the line
//!
//! On a DIO2-only board the host drives nothing, and [`Position::Off`] and
//! [`Position::Receive`] are the same switch position at the pins
//! ([`FrontEnd::lines`]). The state is tracked and the refusals apply there
//! too, deliberately: the driver's call sequence is identical on all three
//! boards, so an ordering bug a T114 cannot feel is still one the solar node
//! would, and this way the host sees it either way.
//!
//! # What it does not do
//!
//! It issues no radio command and decides no timing. DIO2 is the chip's, and
//! the transitions here are recorded beside the commands that move it:
//! [`key_up`](FrontEnd::key_up) immediately before `SetTx`,
//! [`tx_done`](FrontEnd::tx_done) when `TxDone` has put the chip back in
//! standby, [`release`](FrontEnd::release) on every standby and on reset.

/// The board's host-driven RX-enable line.
///
/// One implementation in the firmware, around an `embassy_nrf` output, and a
/// recording fake per test. Writes are unconditional and idempotent, exactly
/// as the driver made them before this type existed.
pub trait RxEnableLine {
    /// Drive the line high (receive path asserted) or low (released).
    fn set_asserted(&mut self, asserted: bool);
}

/// How a board's antenna switch is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wiring {
    /// DIO2 owns the whole switch (T114, RAK4631). The host drives nothing;
    /// with DIO2 low the switch rests on the receive side.
    Dio2Only,
    /// DIO2 steers the transmit side and a host GPIO the receive side
    /// (Wio-SX1262 on the solar node).
    Dio2AndRxEnable,
}

/// Where the front end has been asked to point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// Neither path asked for: after reset, on every standby, after `TxDone`.
    Off,
    /// The receive path asserted, for a listening window or a CAD.
    Receive,
    /// The PA keyed: `SetTx` issued or about to be, DIO2 high until `TxDone`.
    Transmit,
}

/// The levels the switch sees in a [`Position`] on a given [`Wiring`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lines {
    /// The host RX-enable line; `None` on a board that has none.
    pub rx_enable: Option<bool>,
    /// DIO2, the transmit side, which the chip raises with `SetTx`.
    pub tx_path: bool,
}

/// A transition the front end refused because it would ask for both paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontEndFault {
    /// Keying the PA was asked for with the receive path still asserted.
    ReceiveAsserted,
    /// Asserting the receive path was asked for with the PA keyed.
    TransmitKeyed,
}

/// The front-end position and the RX-enable line it drives.
///
/// The two paths are carried as two flags rather than as a [`Position`] so
/// that the invariant is something a transition could break and the refusals
/// are what stop it, not something the type makes trivially true.
pub struct FrontEnd<L> {
    line: Option<L>,
    receive_asserted: bool,
    pa_keyed: bool,
}

impl<L: RxEnableLine> FrontEnd<L> {
    /// A front end in [`Position::Off`]. `line` is the board's RX-enable
    /// line or `None`; it is expected to arrive released (low) and is not
    /// written here.
    pub const fn new(line: Option<L>) -> Self {
        Self {
            line,
            receive_asserted: false,
            pa_keyed: false,
        }
    }

    /// Which of the two switch shapes this board has.
    pub fn wiring(&self) -> Wiring {
        if self.line.is_some() {
            Wiring::Dio2AndRxEnable
        } else {
            Wiring::Dio2Only
        }
    }

    /// Where the front end points now.
    pub fn position(&self) -> Position {
        match (self.receive_asserted, self.pa_keyed) {
            (_, true) => Position::Transmit,
            (true, false) => Position::Receive,
            (false, false) => Position::Off,
        }
    }

    /// The levels at the switch in the current position.
    pub fn lines(&self) -> Lines {
        Lines {
            rx_enable: self.line.as_ref().map(|_| self.receive_asserted),
            tx_path: self.pa_keyed,
        }
    }

    /// Never both paths asked for at once.
    pub const fn consistent(&self) -> bool {
        !(self.receive_asserted && self.pa_keyed)
    }

    /// Assert the receive path, immediately before `SetRx` or `SetCad`.
    ///
    /// Refused with the PA keyed: the chip is transmitting until `TxDone` or
    /// a standby says otherwise, and asserting the receive side then is the
    /// both-at-once state. The line is not written on a refusal.
    pub fn receive(&mut self) -> Result<(), FrontEndFault> {
        if self.pa_keyed {
            return Err(FrontEndFault::TransmitKeyed);
        }
        self.drive(true);
        self.receive_asserted = true;
        Ok(())
    }

    /// Release the receive path and record the PA as unkeyed: the chip has
    /// been put in standby, or reset. Valid from every position.
    pub fn release(&mut self) {
        self.release_receive();
        self.pa_keyed = false;
    }

    /// Off the receive path, then the PA, immediately before `SetTx`.
    ///
    /// The order is the point. Releasing first is what makes the key legal;
    /// the private `key_pa` refuses otherwise, so swapping the two calls
    /// turns every key-up from [`Position::Receive`] into a refusal.
    pub fn key_up(&mut self) -> Result<(), FrontEndFault> {
        self.release_receive();
        self.key_pa()
    }

    /// `TxDone`: the chip is back in standby on its own and DIO2 has fallen.
    /// The RX-enable line was released at the key-up and is not written.
    pub fn tx_done(&mut self) {
        self.pa_keyed = false;
    }

    fn release_receive(&mut self) {
        self.drive(false);
        self.receive_asserted = false;
    }

    fn key_pa(&mut self) -> Result<(), FrontEndFault> {
        if self.receive_asserted {
            return Err(FrontEndFault::ReceiveAsserted);
        }
        self.pa_keyed = true;
        Ok(())
    }

    fn drive(&mut self, asserted: bool) {
        if let Some(line) = self.line.as_mut() {
            line.set_asserted(asserted);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::vec::Vec;

    /// Records every write, shared so the test keeps a handle after the
    /// front end has taken the line.
    #[derive(Clone, Default)]
    struct FakeLine(Rc<RefCell<Vec<bool>>>);

    impl RxEnableLine for FakeLine {
        fn set_asserted(&mut self, asserted: bool) {
            self.0.borrow_mut().push(asserted);
        }
    }

    impl FakeLine {
        fn take(&self) -> Vec<bool> {
            core::mem::take(&mut *self.0.borrow_mut())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Op {
        Receive,
        Release,
        KeyUp,
        TxDone,
    }

    const POSITIONS: [Position; 3] = [Position::Off, Position::Receive, Position::Transmit];
    const OPS: [Op; 4] = [Op::Receive, Op::Release, Op::KeyUp, Op::TxDone];

    fn apply<L: RxEnableLine>(fe: &mut FrontEnd<L>, op: Op) -> Result<(), FrontEndFault> {
        match op {
            Op::Receive => fe.receive(),
            Op::Release => {
                fe.release();
                Ok(())
            }
            Op::KeyUp => fe.key_up(),
            Op::TxDone => {
                fe.tx_done();
                Ok(())
            }
        }
    }

    /// A front end brought to `pos` by the driver's own transitions.
    fn at(pos: Position, line: Option<FakeLine>) -> FrontEnd<FakeLine> {
        let mut fe = FrontEnd::new(line);
        match pos {
            Position::Off => {}
            Position::Receive => fe.receive().unwrap(),
            Position::Transmit => fe.key_up().unwrap(),
        }
        assert_eq!(fe.position(), pos);
        fe
    }

    /// From each position, each transition: the resulting position (or the
    /// refusal) and the RX-enable writes it made, which are the GPIO sequence
    /// the driver issued before this type existed.
    fn expected(from: Position, op: Op) -> (Result<Position, FrontEndFault>, &'static [bool]) {
        use Position::*;
        match (from, op) {
            (_, Op::Release) => (Ok(Off), &[false]),
            (Transmit, Op::Receive) => (Err(FrontEndFault::TransmitKeyed), &[]),
            (_, Op::Receive) => (Ok(Receive), &[true]),
            (_, Op::KeyUp) => (Ok(Transmit), &[false]),
            (Transmit, Op::TxDone) => (Ok(Off), &[]),
            (p, Op::TxDone) => (Ok(p), &[]),
        }
    }

    #[test]
    fn every_transition_pair_lands_where_the_table_says_and_stays_consistent() {
        for wired in [false, true] {
            for from in POSITIONS {
                for op in OPS {
                    let line = FakeLine::default();
                    let mut fe = at(from, wired.then(|| line.clone()));
                    line.take();
                    let (want, writes) = expected(from, op);
                    let got = apply(&mut fe, op).map(|()| fe.position());
                    assert_eq!(got, want, "wired={wired} from={from:?} op={op:?}");
                    if got.is_err() {
                        assert_eq!(fe.position(), from, "a refusal moves nothing");
                    }
                    let want_writes: &[bool] = if wired { writes } else { &[] };
                    assert_eq!(
                        line.take(),
                        want_writes,
                        "wired={wired} from={from:?} op={op:?}"
                    );
                    assert!(fe.consistent(), "wired={wired} from={from:?} op={op:?}");
                }
            }
        }
    }

    #[test]
    fn every_two_step_sequence_keeps_both_paths_apart() {
        for wired in [false, true] {
            for first in OPS {
                for second in OPS {
                    let mut fe = FrontEnd::new(wired.then(FakeLine::default));
                    let _ = apply(&mut fe, first);
                    assert!(fe.consistent());
                    let _ = apply(&mut fe, second);
                    assert!(fe.consistent(), "wired={wired} {first:?} then {second:?}");
                    if let Some(rx) = fe.lines().rx_enable {
                        assert!(!(rx && fe.lines().tx_path));
                    }
                }
            }
        }
    }

    #[test]
    fn the_line_follows_the_position_on_the_rx_enable_board() {
        let want = [
            (Position::Off, Some(false), false),
            (Position::Receive, Some(true), false),
            (Position::Transmit, Some(false), true),
        ];
        for (pos, rx_enable, tx_path) in want {
            let fe = at(pos, Some(FakeLine::default()));
            assert_eq!(fe.wiring(), Wiring::Dio2AndRxEnable);
            assert_eq!(fe.lines(), Lines { rx_enable, tx_path }, "{pos:?}");
        }
    }

    /// DIO2-only: the host has no line, so Off and Receive are one switch
    /// position at the pins and only DIO2 tells Transmit apart.
    #[test]
    fn the_dio2_only_board_has_two_switch_positions_for_three_states() {
        let want = [
            (Position::Off, false),
            (Position::Receive, false),
            (Position::Transmit, true),
        ];
        for (pos, tx_path) in want {
            let fe = at(pos, None);
            assert_eq!(fe.wiring(), Wiring::Dio2Only);
            assert_eq!(
                fe.lines(),
                Lines {
                    rx_enable: None,
                    tx_path
                },
                "{pos:?}"
            );
        }
    }

    #[test]
    fn arming_the_receiver_under_a_keyed_pa_is_refused_without_a_write() {
        let line = FakeLine::default();
        let mut fe = at(Position::Transmit, Some(line.clone()));
        line.take();
        assert_eq!(fe.receive(), Err(FrontEndFault::TransmitKeyed));
        assert_eq!(line.take(), Vec::<bool>::new());
        assert_eq!(fe.position(), Position::Transmit);
    }

    #[test]
    fn keying_the_pa_with_the_receive_path_asserted_is_refused() {
        for wired in [false, true] {
            let mut fe = at(Position::Receive, wired.then(FakeLine::default));
            assert_eq!(fe.key_pa(), Err(FrontEndFault::ReceiveAsserted));
            assert_eq!(fe.position(), Position::Receive);
            assert!(fe.consistent());
        }
    }

    /// The listen-CAD-transmit cycle as the driver runs it on the solar
    /// node, written out as the RX-enable levels the pin sees.
    #[test]
    fn a_cad_then_a_key_up_releases_before_the_pa_and_a_tx_done_leaves_it_released() {
        let line = FakeLine::default();
        let mut fe = FrontEnd::new(Some(line.clone()));
        fe.release(); // reset
        fe.release(); // standby in init
        fe.receive().unwrap(); // SetRx
        fe.release(); // standby before the CAD
        fe.receive().unwrap(); // SetCad
        fe.key_up().unwrap(); // clear channel, SetTx
        assert_eq!(
            fe.lines(),
            Lines {
                rx_enable: Some(false),
                tx_path: true
            }
        );
        fe.tx_done();
        fe.receive().unwrap(); // the next window
        assert_eq!(line.take(), [false, false, true, false, true, false, true]);
        assert_eq!(fe.position(), Position::Receive);
    }
}
