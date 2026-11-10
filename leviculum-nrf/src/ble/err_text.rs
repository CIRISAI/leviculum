//! The `err=` field of the central's failure lines (`BLE_CENTRAL_FAIL`,
//! `BLE_TX_DROP`, the scan warning), written without nrf-softdevice's
//! `GattError` `Debug`.
//!
//! The derived `Debug` of the softdevice's error enums prints a GATT status
//! through `GattStatus`'s `Debug`, a match over every ATT and GATT status the
//! SDK names with a phrase per arm. On the RAK4631 that table and the
//! `Debug` impls around it were 2.3 KB of an image with 10.7 KB left; this
//! module wins 1240 B of it back (order 1620).
//!
//! Everything here prints what the derive printed, variant names and
//! `Raw(..)` through `RawError`'s own `Debug` (the softdevice's `unwrap!`
//! keeps that one linked anyway), except a GATT status: it is its number,
//! `Gatt(0x0103)`, the SDK's `BLE_GATT_STATUS_*` value (ATT error codes are
//! `0x0100` plus the Bluetooth Core error code), instead of its phrase.
//!
//! No reader parses the field: periculum's BLE cells name the lines in their
//! comments and assert nothing on `err=`.

use core::fmt;

use nrf_softdevice::ble::central::{ConnectError, ScanError};
use nrf_softdevice::ble::gatt_client::{DiscoverError, MtuExchangeError, ReadError, WriteError};
use nrf_softdevice::ble::GattError;
use nrf_softdevice::RawError;

/// An error of the central's dial, printed for the `err=` field.
pub struct ErrText<'a, E>(pub &'a E);

fn gatt(f: &mut fmt::Formatter<'_>, err: GattError) -> fmt::Result {
    write!(f, "Gatt(0x{:04x})", u16::from(err))
}

fn raw(f: &mut fmt::Formatter<'_>, err: &RawError) -> fmt::Result {
    write!(f, "Raw({:?})", err)
}

impl fmt::Display for ErrText<'_, MtuExchangeError> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            MtuExchangeError::Disconnected => f.write_str("Disconnected"),
            MtuExchangeError::Gatt(e) => gatt(f, *e),
            MtuExchangeError::Raw(e) => raw(f, e),
        }
    }
}

impl fmt::Display for ErrText<'_, ConnectError> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ConnectError::Timeout => f.write_str("Timeout"),
            ConnectError::NoAddresses => f.write_str("NoAddresses"),
            ConnectError::NoFreeConn => f.write_str("NoFreeConn"),
            ConnectError::MtuExchange(e) => write!(f, "MtuExchange({})", ErrText(e)),
            ConnectError::Raw(e) => raw(f, e),
        }
    }
}

impl fmt::Display for ErrText<'_, ScanError> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ScanError::Timeout => f.write_str("Timeout"),
            ScanError::Raw(e) => raw(f, e),
        }
    }
}

impl fmt::Display for ErrText<'_, DiscoverError> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            DiscoverError::Disconnected => f.write_str("Disconnected"),
            DiscoverError::ServiceNotFound => f.write_str("ServiceNotFound"),
            DiscoverError::ServiceIncomplete => f.write_str("ServiceIncomplete"),
            DiscoverError::Gatt(e) => gatt(f, *e),
            DiscoverError::Raw(e) => raw(f, e),
        }
    }
}

impl fmt::Display for ErrText<'_, ReadError> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ReadError::Disconnected => f.write_str("Disconnected"),
            ReadError::Truncated => f.write_str("Truncated"),
            ReadError::Gatt(e) => gatt(f, *e),
            ReadError::Raw(e) => raw(f, e),
        }
    }
}

impl fmt::Display for ErrText<'_, WriteError> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            WriteError::Disconnected => f.write_str("Disconnected"),
            WriteError::Timeout => f.write_str("Timeout"),
            WriteError::Gatt(e) => gatt(f, *e),
            WriteError::Raw(e) => raw(f, e),
        }
    }
}
