//! What the board's BLE paths do when the operator switches the carrier
//! off (leviculum#413, census tier C: rows 83, 91, 101, 116, 144, 146 and
//! 150).
//!
//! `lnflash --set-media ble=off` on a live board reaches seven places:
//! the peripheral advertising tasks, the central scan task, every live
//! session on either role, the interface's outbound path, and the waiter
//! that wakes them. Each used to read the media state and decide inline;
//! they must all agree the carrier is gone, so each asks [`CarrierState`]
//! for its [`CarrierPath`] and acts on the one verdict.
//!
//! On carrier off: advertising stops, scanning stops, a session ends
//! with the carrier as its reason, an outbound packet is swallowed as
//! `Ok` and counted in the carrier's drop run, and none of it books a
//! backoff, there being nothing to pace against while off.

use crate::{scan_pass_pause_ms, ScanPass};

/// The BLE carrier as the media profile states it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarrierState {
    on: bool,
}

/// A place on the board that reads the carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierPath {
    /// A peripheral task's gate, before it advertises (row 101).
    AdvertiseGate,
    /// A peripheral task's advertisement in flight (row 83's waiter).
    Advertising,
    /// The central task's gate, before it scans (row 146).
    ScanGate,
    /// The central task's scan in flight (row 150).
    Scanning,
    /// A live session, either role (rows 116 and 144).
    Session,
    /// The interface's outbound path (row 91).
    Outbound,
}

impl CarrierPath {
    /// Every path, for the tests and for a census that counts them.
    pub const ALL: [Self; 6] = [
        Self::AdvertiseGate,
        Self::Advertising,
        Self::ScanGate,
        Self::Scanning,
        Self::Session,
        Self::Outbound,
    ];
}

/// What a path does with the carrier as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierStep {
    /// The carrier is on: go on.
    Proceed,
    /// The carrier is off: stand down this way.
    StandDown(StandDown),
}

/// How a path stands down on a carrier switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandDown {
    /// Stop advertising (dropping the advertise future cancels it) and
    /// hold at the gate until the carrier is on.
    StopAdvertising,
    /// Stop scanning (dropping the scan future stops it) and hold at the
    /// gate until the carrier is on.
    StopScanning,
    /// End the session: `BLE_CARRIER_DROP`, disconnect, and the
    /// teardown range loss takes, so the core's cull is the same.
    EndSession,
    /// Answer the core `Ok` and count the packet in the carrier's drop
    /// run: a media profile is the operator's choice, not a fault the
    /// core should retry against.
    Swallow,
}

impl StandDown {
    /// Whether standing down disconnects a link.
    #[must_use]
    pub const fn disconnects(self) -> bool {
        matches!(self, Self::EndSession)
    }

    /// Whether the core is answered `Ok`.
    #[must_use]
    pub const fn answers_ok(self) -> bool {
        matches!(self, Self::Swallow)
    }

    /// The pause booked before the path runs again: none on any path.
    /// The gate holds a task until the carrier is on, and a pass that
    /// ended on the carrier is [`ScanPass::CarrierOff`], which
    /// [`scan_pass_pause_ms`] does not pace.
    #[must_use]
    pub fn pause_ms(self) -> Option<u64> {
        match self {
            Self::StopScanning => scan_pass_pause_ms(ScanPass::CarrierOff),
            Self::StopAdvertising | Self::EndSession | Self::Swallow => None,
        }
    }
}

impl CarrierState {
    /// The carrier as `ble_active` reads it.
    #[must_use]
    pub const fn from_active(on: bool) -> Self {
        Self { on }
    }

    /// Whether the carrier is on. The interface's peer count and its
    /// online state read zero and offline with it off (rows 88 and 89).
    #[must_use]
    pub const fn is_on(self) -> bool {
        self.on
    }

    /// The verdict for one path.
    #[must_use]
    pub const fn step(self, path: CarrierPath) -> CarrierStep {
        if self.on {
            return CarrierStep::Proceed;
        }
        CarrierStep::StandDown(match path {
            CarrierPath::AdvertiseGate | CarrierPath::Advertising => StandDown::StopAdvertising,
            CarrierPath::ScanGate | CarrierPath::Scanning => StandDown::StopScanning,
            CarrierPath::Session => StandDown::EndSession,
            CarrierPath::Outbound => StandDown::Swallow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_proceeds_with_the_carrier_on() {
        let on = CarrierState::from_active(true);
        for path in CarrierPath::ALL {
            assert_eq!(on.step(path), CarrierStep::Proceed, "{path:?}");
        }
    }

    #[test]
    fn every_path_stands_down_with_the_carrier_off_and_none_books_a_backoff() {
        let off = CarrierState::from_active(false);
        for path in CarrierPath::ALL {
            let CarrierStep::StandDown(down) = off.step(path) else {
                panic!("{path:?} proceeded with the carrier off");
            };
            assert_eq!(down.pause_ms(), None, "{path:?}");
        }
    }

    #[test]
    fn advertising_and_scanning_stop_at_the_gate_and_in_flight_alike() {
        let off = CarrierState::from_active(false);
        for path in [CarrierPath::AdvertiseGate, CarrierPath::Advertising] {
            assert_eq!(
                off.step(path),
                CarrierStep::StandDown(StandDown::StopAdvertising)
            );
        }
        for path in [CarrierPath::ScanGate, CarrierPath::Scanning] {
            assert_eq!(
                off.step(path),
                CarrierStep::StandDown(StandDown::StopScanning)
            );
        }
    }

    #[test]
    fn only_a_session_disconnects_and_only_the_outbound_path_answers_ok() {
        let off = CarrierState::from_active(false);
        for path in CarrierPath::ALL {
            let CarrierStep::StandDown(down) = off.step(path) else {
                panic!("{path:?} proceeded with the carrier off");
            };
            assert_eq!(down.disconnects(), path == CarrierPath::Session, "{path:?}");
            assert_eq!(down.answers_ok(), path == CarrierPath::Outbound, "{path:?}");
        }
    }

    #[test]
    fn the_interface_reads_no_peer_and_offline_with_the_carrier_off() {
        assert!(!CarrierState::from_active(false).is_on());
        assert!(CarrierState::from_active(true).is_on());
    }
}
