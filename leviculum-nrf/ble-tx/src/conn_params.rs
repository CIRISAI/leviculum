//! What a link actually runs at: connection interval, slave latency
//! and supervision timeout (Codeberg #385).
//!
//! Neither stack asks for connection parameters, so both inherit
//! whatever the central picks, and until this module nothing wrote the
//! inherited values down. The parameters are not cosmetic: the
//! supervision timeout is exactly how long a disturbance has to last
//! before the link dies, and the interval bounds how fast anything can
//! be sent or acknowledged. On the bench the values were readable only
//! because the central was BlueZ and its kernel could be inspected —
//! `36 × 1.25 ms` interval, `42 × 10 ms` timeout, 36 link deaths in
//! 16.2 hours with HCI reason 0x08. Against a phone there is no
//! equivalent question to ask, so the link has to say it itself.
//!
//! # The two scales
//!
//! The SoftDevice (and the Bluetooth spec it follows) reports the two
//! time-valued fields in DIFFERENT units: the connection interval in
//! 1.25 ms steps, the supervision timeout in 10 ms steps. A log line
//! that printed the raw numbers would force every reader of every field
//! capture to remember which scale belongs to which field, and a reader
//! who misremembers reads `timeout=420` as 4.2 seconds instead of
//! 420 ms. Both are therefore converted here, once, and the line
//! carries milliseconds only.
//!
//! The interval conversion is exact in hundredths of a millisecond
//! (`raw × 1.25 ms = raw × 125 / 100 ms`), so nothing is rounded away:
//! the smallest legal interval, 6 units, prints as `7.50` and not as
//! `7` or `8`.
//!
//! The conversion lives in this crate and not in the firmware for the
//! usual reason — the firmware crate cross-compiles and runs no host
//! tests, so a wrong scale there would be found by a reader of a field
//! log, months later, if at all. Here it fails a test.

use core::fmt;

/// Which end of the link we are, in the spelling the log line uses.
///
/// The same distinction [`crate::registry::Origin`] draws for duplicate
/// judging (`Incoming` = we are the peripheral), but the field it feeds
/// is `role=` and its two words are `central`/`peripheral`, so the
/// mapping is stated at the call site rather than smuggled into
/// `Origin`'s own log spelling (`origin=incoming|outgoing`), which
/// several existing lines already carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRole {
    /// We initiated the connection; the interval, latency and timeout
    /// are ours to pick (and today we do not pick them).
    Central,
    /// The peer initiated; every parameter below was chosen by them.
    Peripheral,
}

impl LinkRole {
    /// The `role=` token.
    pub const fn as_str(self) -> &'static str {
        match self {
            LinkRole::Central => "central",
            LinkRole::Peripheral => "peripheral",
        }
    }
}

/// One connection's live parameters, in the raw units the controller
/// reports them in.
///
/// Constructed from `ble_gap_conn_params_t` on the firmware side. Both
/// `min_conn_interval` and `max_conn_interval` are equal to the actual
/// interval whenever the struct arrives in an event (SoftDevice S140
/// bindings, `ble_gap_conn_params_t` doc comment: "When ble_conn_params_t
/// is received in an event, both min_conn_interval and max_conn_interval
/// will be equal to the connection interval set by the central"), which
/// is the only way this type is ever built — so it holds ONE interval,
/// not a range, and a reader of the line never has to wonder which of
/// two numbers the link is running at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnParams {
    /// Connection interval in 1.25 ms units (legal range 6..=3200).
    pub interval_units: u16,
    /// Slave latency: connection events the peripheral may skip
    /// (legal range 0..=499). Unitless, so it is logged unconverted.
    pub latency: u16,
    /// Supervision timeout in 10 ms units (legal range 10..=3200).
    pub timeout_units: u16,
}

impl ConnParams {
    /// The connection interval in hundredths of a millisecond.
    ///
    /// Hundredths rather than milliseconds because the unit is 1.25 ms:
    /// every legal value is exact in hundredths and only a quarter of
    /// them are exact in whole milliseconds. The maximum, 3200 units,
    /// gives 400_000, so `u32` never overflows.
    pub const fn interval_hundredths_ms(self) -> u32 {
        self.interval_units as u32 * 125
    }

    /// The supervision timeout in whole milliseconds. The 10 ms unit
    /// makes every legal value exact; the maximum, 3200 units, is 32 s.
    pub const fn timeout_ms(self) -> u32 {
        self.timeout_units as u32 * 10
    }
}

/// The `BLE_CONN_PARAMS` line's body, byte-exact.
///
/// Rendered through [`fmt::Display`] so both stacks emit the identical
/// shape from the identical code, and so a host test can assert the
/// bytes a field capture will be grepped for. The trailing `t=<ms>`
/// stamp is NOT part of it: on the board the log layer appends it to
/// every line, and duplicating it here would produce two stamps.
pub struct ConnParamsLine {
    /// The connection handle, matching the `conn=` of every other
    /// per-link line (`BLE_TX_GAP`, `BLE_LINK_DUP`, `BLE: RX`).
    pub conn: u16,
    pub role: LinkRole,
    pub params: ConnParams,
}

impl fmt::Display for ConnParamsLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hundredths = self.params.interval_hundredths_ms();
        write!(
            f,
            "BLE_CONN_PARAMS conn={} role={} interval_ms={}.{:02} latency={} timeout_ms={}",
            self.conn,
            self.role.as_str(),
            hundredths / 100,
            hundredths % 100,
            self.params.latency,
            self.params.timeout_ms(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    /// The bench connection #385 measured: 36 units interval, no
    /// latency, 42 units supervision timeout. The two numbers the
    /// issue quotes as 45 ms and 420 ms have to come out as those.
    #[test]
    fn bench_connection_converts_to_the_measured_milliseconds() {
        let p = ConnParams {
            interval_units: 36,
            latency: 0,
            timeout_units: 42,
        };
        assert_eq!(p.interval_hundredths_ms(), 4500);
        assert_eq!(p.timeout_ms(), 420);
    }

    /// Interval, lower bound: 6 units is the smallest the spec allows,
    /// and it is 7.5 ms — the value that catches a conversion which
    /// rounds to whole milliseconds.
    #[test]
    fn interval_lower_bound_is_seven_and_a_half_ms() {
        let p = ConnParams {
            interval_units: 6,
            latency: 0,
            timeout_units: 10,
        };
        assert_eq!(p.interval_hundredths_ms(), 750);
    }

    /// Interval, upper bound: 3200 units = 4000 ms exactly, and the
    /// product must not overflow the accumulator.
    #[test]
    fn interval_upper_bound_is_four_seconds() {
        let p = ConnParams {
            interval_units: 3200,
            latency: 0,
            timeout_units: 3200,
        };
        assert_eq!(p.interval_hundredths_ms(), 400_000);
    }

    /// A `u16` full of ones is not a legal interval, but a controller
    /// that reports one must not make the conversion wrap. 65535 × 125
    /// is 8_191_875, inside `u32`.
    #[test]
    fn interval_does_not_overflow_on_an_illegal_maximum() {
        let p = ConnParams {
            interval_units: u16::MAX,
            latency: 0,
            timeout_units: u16::MAX,
        };
        assert_eq!(p.interval_hundredths_ms(), 8_191_875);
        assert_eq!(p.timeout_ms(), 655_350);
    }

    /// Timeout bounds: 10 units = 100 ms (the smallest legal), 3200
    /// units = 32 s (the largest).
    #[test]
    fn timeout_bounds_are_a_tenth_of_a_second_and_thirty_two_seconds() {
        assert_eq!(
            ConnParams {
                interval_units: 6,
                latency: 0,
                timeout_units: 10,
            }
            .timeout_ms(),
            100
        );
        assert_eq!(
            ConnParams {
                interval_units: 6,
                latency: 0,
                timeout_units: 3200,
            }
            .timeout_ms(),
            32_000
        );
    }

    /// Latency passes through untouched at both ends of its legal
    /// range: it counts connection events, it is not a time.
    #[test]
    fn latency_is_not_converted() {
        for latency in [0, 499] {
            let line = ConnParamsLine {
                conn: 1,
                role: LinkRole::Central,
                params: ConnParams {
                    interval_units: 6,
                    latency,
                    timeout_units: 10,
                },
            };
            assert!(line
                .to_string()
                .contains(&std::format!("latency={latency} ")));
        }
    }

    /// The whole line, byte for byte, as a capture will be grepped for.
    #[test]
    fn line_is_byte_exact() {
        let line = ConnParamsLine {
            conn: 3,
            role: LinkRole::Peripheral,
            params: ConnParams {
                interval_units: 36,
                latency: 0,
                timeout_units: 42,
            },
        };
        assert_eq!(
            line.to_string(),
            "BLE_CONN_PARAMS conn=3 role=peripheral interval_ms=45.00 latency=0 timeout_ms=420"
        );
    }

    /// A fractional interval keeps both decimals, and a sub-10
    /// hundredths remainder keeps its leading zero — 9 units is
    /// 11.25 ms, 7 units is 8.75 ms, and 1601 units is 2001.25 ms.
    /// Without the `{:02}` the first would print as `11.25`, the
    /// second as `8.75` and a value like 8 units (10.00 ms) as
    /// `10.0`.
    #[test]
    fn line_keeps_two_decimals() {
        let render = |units| {
            ConnParamsLine {
                conn: 0,
                role: LinkRole::Central,
                params: ConnParams {
                    interval_units: units,
                    latency: 0,
                    timeout_units: 10,
                },
            }
            .to_string()
        };
        assert!(render(9).contains("interval_ms=11.25 "));
        assert!(render(7).contains("interval_ms=8.75 "));
        assert!(render(8).contains("interval_ms=10.00 "));
        assert!(render(1601).contains("interval_ms=2001.25 "));
    }

    /// Both roles spell themselves the way the line's reader expects.
    #[test]
    fn roles_spell_themselves() {
        assert_eq!(LinkRole::Central.as_str(), "central");
        assert_eq!(LinkRole::Peripheral.as_str(), "peripheral");
    }
}
