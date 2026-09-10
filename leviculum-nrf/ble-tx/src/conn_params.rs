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
//!
//! # Asking for a timeout the link can survive
//!
//! The measurement half of #385 produced two numbers that do not sit
//! well together: a link opened by lnsd runs at `interval_ms=45.00
//! timeout_ms=420`, and 420 ms at a 45 ms interval is about nine
//! connection events. Nine missed events end the link. Board-to-board
//! links do not have the problem — `ConnectConfig::default` asks for
//! 4 s in the central role — and the host cannot set these values
//! through the API lnsd uses at all, so the only end that can do
//! anything about it is the peripheral, which is exactly what our
//! board is in the two cases that are bad (lnsd, 420 ms) or unknown
//! (a phone, unmeasured).
//!
//! [`judge_supervision_timeout`] is that decision, and it is
//! deliberately CONDITIONAL: it reads the value it is about to act on
//! every time, and asks for nothing when the link already came up with
//! a timeout it can survive.
//!
//! ## What is asked for: 4000 ms
//!
//! The same 4 s that `ConnectConfig::default` asks for in the central
//! role, so the two roles agree on one number instead of the stack
//! carrying a third. It is bounded on both sides by facts rather than
//! by custom:
//!
//! - From above by our own link expiry. `registry::LINK_TIMEOUT_MS` is
//!   45 s, and a supervision timeout is exactly how long it takes the
//!   controller to notice a peer that has genuinely gone. At 4 s the
//!   controller notices a decade of seconds before the keepalive clock
//!   would, so the two mechanisms never race to explain the same
//!   death; a timeout up near 45 s would put them in a photo finish,
//!   and one at 32 s (the largest the spec allows) would delay the
//!   news of a dead peer by 32 s for nothing.
//! - From below by what it has to survive. 420 ms is about nine
//!   connection events at the measured interval; 4000 ms is about
//!   ninety.
//!
//! ## The floor: 2000 ms
//!
//! The floor is NOT the requested value, and the gap between them is
//! the whole point. A central that already negotiates something sane
//! must be left alone — an update request that fights a good value is
//! a regression, and no board carrying this build has yet been near a
//! phone, so what Columba picks is not knowable here. With the floor
//! set equal to the request, a link that came up at 3900 ms would be
//! interrupted to gain 100 ms, and one at 5000 ms would be dragged
//! DOWN to 4000.
//!
//! Half the requested value is where the two considerations cross.
//! Between 2 s and 4 s a link is within a factor of two of what we
//! would ask for, so the most a request could win is that factor —
//! which does not pay for the risk of a refusal, or of a central that
//! answers by renegotiating something worse than what it had. Below
//! 2 s the distance is more than a factor of two, and 420 ms, the
//! value that started this, is off by nearly ten.
//!
//! ## The ask is always arithmetically legal when it is made
//!
//! The spec bounds a supervision timeout from below by the interval:
//! it must exceed `(1 + latency) × interval × 2`. Any measured set
//! that is itself legal AND below the 2000 ms floor therefore has
//! `(1 + latency) × interval × 2 < 2000 < 4000`, so 4000 ms is legal
//! at that same interval and latency — the ask can never be rejected
//! as arithmetic. The timeout is asked for alone for the same kind of
//! reason: interval and latency are copied from what the link already
//! runs at, because we have no complaint about either, and a request
//! that also moved a field we do not care about could be refused over
//! that field.

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
    ///
    /// Passed in rather than read off the `Connection` at render time:
    /// at [`LinkPhase::Close`] the handle is already gone (the
    /// SoftDevice clears it on the disconnect event, unlike the
    /// parameters themselves), and a close line that could not be
    /// matched to its open line would say nothing about whether the
    /// request was honoured.
    pub conn: u16,
    pub role: LinkRole,
    pub params: ConnParams,
    /// Connect or teardown; see [`LinkPhase`].
    pub phase: LinkPhase,
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
        )?;
        f.write_str(self.phase.suffix())
    }
}

/// The supervision timeout a peripheral link has to reach before it is
/// left alone, in milliseconds. Half of
/// [`REQUESTED_SUPERVISION_TIMEOUT_UNITS`]; the module comment says why
/// the two are not the same number.
pub const SUPERVISION_TIMEOUT_FLOOR_MS: u32 = 2_000;

/// What a peripheral below the floor asks for, in the SoftDevice's
/// 10 ms units: 400 units = 4000 ms, the value
/// `central::ConnectConfig::default` already asks for in the central
/// role.
pub const REQUESTED_SUPERVISION_TIMEOUT_UNITS: u16 = 400;

/// The verdict of [`judge_supervision_timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnParamsAsk {
    /// The link came up with a timeout at or above
    /// [`SUPERVISION_TIMEOUT_FLOOR_MS`]: ask for nothing. Carries the
    /// value that passed, because that value — and not the one we would
    /// have asked for — is what the `skipped` log line has to state.
    Keep {
        /// The measured supervision timeout, in milliseconds.
        timeout_ms: u32,
    },
    /// Below the floor: send a connection parameter update request for
    /// exactly this set. `interval_units` and `latency` are the
    /// measured ones; only the timeout differs.
    Ask(ConnParams),
}

/// Should this peripheral link ask its central for a longer
/// supervision timeout?
///
/// A pure function of the parameters the link actually came up at, so
/// the rule is exercised on the host rather than only through the
/// SoftDevice, and so the boundary can be tested from both sides. The
/// firmware's only job is to read `conn.conn_params()`, hand the
/// numbers here, and perform the verdict.
pub const fn judge_supervision_timeout(measured: ConnParams) -> ConnParamsAsk {
    let timeout_ms = measured.timeout_ms();
    if timeout_ms >= SUPERVISION_TIMEOUT_FLOOR_MS {
        ConnParamsAsk::Keep { timeout_ms }
    } else {
        ConnParamsAsk::Ask(ConnParams {
            interval_units: measured.interval_units,
            latency: measured.latency,
            timeout_units: REQUESTED_SUPERVISION_TIMEOUT_UNITS,
        })
    }
}

/// What became of the request, for the `result=` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnParamsReq {
    /// The request left the board. NOT that it was granted: on a
    /// peripheral `set_conn_params` only starts the L2CAP connection
    /// parameter update procedure, and the central may honour it,
    /// ignore it, or answer with something else entirely. Which of the
    /// three happened is readable only from the `when=close` line.
    Sent,
    /// The SoftDevice would not take the request — a busy link, a
    /// connection already gone. Local, and not retried: the link runs
    /// on at the timeout it has.
    Refused,
    /// Not asked for: the value the link came up at already passes
    /// [`SUPERVISION_TIMEOUT_FLOOR_MS`].
    Skipped,
}

impl ConnParamsReq {
    /// The `result=` token.
    pub const fn as_str(self) -> &'static str {
        match self {
            ConnParamsReq::Sent => "sent",
            ConnParamsReq::Refused => "refused",
            ConnParamsReq::Skipped => "skipped",
        }
    }
}

/// The `BLE_CONN_PARAMS_REQ` line's body, byte-exact — the decision
/// [`judge_supervision_timeout`] made and what came of it.
///
/// One line per peripheral link, always: a link that asks and a link
/// that does not both say so, because "no line" is indistinguishable
/// from "this build does not have the feature" in a field capture.
pub struct ConnParamsReqLine {
    /// The connection handle, matching the `conn=` of the
    /// `BLE_CONN_PARAMS` line the decision was made from.
    pub conn: u16,
    /// The supervision timeout this line is about, in milliseconds: on
    /// `sent`/`refused` the value asked for, on `skipped` the value
    /// that passed the floor. Both are the number a reader wants next
    /// to that word, and [`ConnParamsAsk`] hands the right one to each.
    pub timeout_ms: u32,
    pub result: ConnParamsReq,
}

impl fmt::Display for ConnParamsReqLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BLE_CONN_PARAMS_REQ conn={} timeout_ms={} result={}",
            self.conn,
            self.timeout_ms,
            self.result.as_str(),
        )
    }
}

/// Which end of a link's life a [`ConnParamsLine`] was read at.
///
/// The accepted values are what matter, not the request, so the line is
/// emitted twice: once when the link comes up, once when it ends. A
/// link that opened at 420 ms and closed at 4000 ms says the request
/// was honoured; one that closed at 420 ms says it was not; neither is
/// knowable any other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPhase {
    /// Read at connect. Renders NO marker, so the line stays
    /// byte-identical to the one `a7456002` measured and the captures
    /// grepped against it keep matching.
    Open,
    /// Re-read as the link ends, next to the disconnect line where
    /// `att_mtu()` is already re-read for the same reason.
    Close,
}

impl LinkPhase {
    /// What the phase appends to the line — a leading space and the
    /// `when=` token, or nothing at all.
    pub const fn suffix(self) -> &'static str {
        match self {
            LinkPhase::Open => "",
            LinkPhase::Close => " when=close",
        }
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
                phase: LinkPhase::Open,
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
            phase: LinkPhase::Open,
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
                phase: LinkPhase::Open,
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

    /// The measured bench case, the one this half of #385 exists for:
    /// 420 ms is below the floor, so the link asks — and it asks for
    /// 4000 ms while leaving the interval and the latency exactly as
    /// the central set them.
    #[test]
    fn the_measured_bench_link_asks_for_four_seconds() {
        let measured = ConnParams {
            interval_units: 36,
            latency: 0,
            timeout_units: 42,
        };
        assert_eq!(
            judge_supervision_timeout(measured),
            ConnParamsAsk::Ask(ConnParams {
                interval_units: 36,
                latency: 0,
                timeout_units: 400,
            })
        );
        match judge_supervision_timeout(measured) {
            ConnParamsAsk::Ask(asked) => assert_eq!(asked.timeout_ms(), 4000),
            other => panic!("expected an ask, got {other:?}"),
        }
    }

    /// The boundary, from below: one unit under the floor is 1990 ms
    /// and asks.
    #[test]
    fn one_unit_below_the_floor_asks() {
        let measured = ConnParams {
            interval_units: 36,
            latency: 0,
            timeout_units: 199,
        };
        assert_eq!(measured.timeout_ms(), SUPERVISION_TIMEOUT_FLOOR_MS - 10);
        assert!(matches!(
            judge_supervision_timeout(measured),
            ConnParamsAsk::Ask(_)
        ));
    }

    /// The boundary, from above: exactly the floor is good enough, and
    /// the verdict carries the value that passed rather than the one
    /// that would have been asked for. "At or above", not "above" — a
    /// link sitting precisely on the number we picked as sufficient is
    /// sufficient by construction, and interrupting it would gain the
    /// factor of two the floor was chosen to forgo.
    #[test]
    fn exactly_the_floor_is_kept() {
        let measured = ConnParams {
            interval_units: 36,
            latency: 0,
            timeout_units: 200,
        };
        assert_eq!(measured.timeout_ms(), SUPERVISION_TIMEOUT_FLOOR_MS);
        assert_eq!(
            judge_supervision_timeout(measured),
            ConnParamsAsk::Keep {
                timeout_ms: SUPERVISION_TIMEOUT_FLOOR_MS
            }
        );
    }

    /// A central that already negotiates BETTER than we would ask for
    /// is left alone — the request is conditional precisely so a good
    /// value is never dragged down to ours. 5000 ms and the spec's
    /// maximum 32 s both keep.
    #[test]
    fn a_better_timeout_than_ours_is_never_fought() {
        for units in [500, 3200] {
            let measured = ConnParams {
                interval_units: 36,
                latency: 0,
                timeout_units: units,
            };
            assert_eq!(
                judge_supervision_timeout(measured),
                ConnParamsAsk::Keep {
                    timeout_ms: u32::from(units) * 10
                }
            );
        }
    }

    /// The floor is half the requested value, and the requested value
    /// is what `ConnectConfig::default` asks for in the central role.
    /// Both numbers are load-bearing in the module comment's argument;
    /// a change to either without a change to that argument fails here.
    #[test]
    fn floor_is_half_of_what_is_asked_for() {
        let asked_ms = u32::from(REQUESTED_SUPERVISION_TIMEOUT_UNITS) * 10;
        assert_eq!(asked_ms, 4_000);
        assert_eq!(SUPERVISION_TIMEOUT_FLOOR_MS * 2, asked_ms);
    }

    /// The ask stays well inside our own link expiry: whatever the
    /// controller notices, it notices long before `LINK_TIMEOUT_MS`
    /// (45 s), so the supervision timeout and the keepalive clock never
    /// race to explain the same dead peer.
    #[test]
    fn the_ask_stays_well_inside_our_own_link_expiry() {
        let asked_ms = u64::from(REQUESTED_SUPERVISION_TIMEOUT_UNITS) * 10;
        assert!(
            asked_ms * 4 < crate::registry::LINK_TIMEOUT_MS,
            "asked {asked_ms} ms is not comfortably inside {} ms",
            crate::registry::LINK_TIMEOUT_MS
        );
    }

    /// Whenever the rule decides to ask, what it asks for is legal at
    /// the link's own interval: the spec's lower bound on a supervision
    /// timeout is `(1 + latency) × interval × 2`, and a measured set
    /// that is itself legal and below the floor cannot have a bound
    /// above the floor — let alone above the 4000 ms asked for. Swept
    /// over the interval range and a spread of latencies, keeping only
    /// the sets a controller could legally report.
    #[test]
    fn what_is_asked_for_is_legal_wherever_it_is_asked() {
        let mut asked = 0;
        for interval_units in (6..=3200).step_by(7) {
            for latency in [0, 1, 4, 100, 499] {
                // The spec's bound on the measured set itself, in ms.
                let bound = (u64::from(latency) + 1) * u64::from(interval_units) * 125 * 2 / 100;
                for timeout_units in [10, 41, 42, 100, 199, 200, 400, 3200] {
                    let measured = ConnParams {
                        interval_units,
                        latency,
                        timeout_units,
                    };
                    if u64::from(measured.timeout_ms()) <= bound {
                        continue; // not a set any controller may report
                    }
                    if let ConnParamsAsk::Ask(ask) = judge_supervision_timeout(measured) {
                        assert!(
                            u64::from(ask.timeout_ms()) > bound,
                            "asked {} ms at interval {interval_units} latency {latency}, \
                             below the spec bound {bound} ms",
                            ask.timeout_ms()
                        );
                        asked += 1;
                    }
                }
            }
        }
        // A sweep that never reached the asking branch would prove
        // nothing at all.
        assert!(asked > 0, "the sweep never exercised an ask");
    }

    /// All three outcomes render byte-exactly, and the number next to
    /// each word is the one that word is about.
    #[test]
    fn request_line_is_byte_exact() {
        assert_eq!(
            ConnParamsReqLine {
                conn: 1,
                timeout_ms: 4000,
                result: ConnParamsReq::Sent,
            }
            .to_string(),
            "BLE_CONN_PARAMS_REQ conn=1 timeout_ms=4000 result=sent"
        );
        assert_eq!(
            ConnParamsReqLine {
                conn: 1,
                timeout_ms: 4000,
                result: ConnParamsReq::Refused,
            }
            .to_string(),
            "BLE_CONN_PARAMS_REQ conn=1 timeout_ms=4000 result=refused"
        );
        assert_eq!(
            ConnParamsReqLine {
                conn: 2,
                timeout_ms: 5000,
                result: ConnParamsReq::Skipped,
            }
            .to_string(),
            "BLE_CONN_PARAMS_REQ conn=2 timeout_ms=5000 result=skipped"
        );
    }

    /// The close line is the acceptance evidence, so it has to be
    /// tellable from the open line and matchable to it: same `conn=`,
    /// plus the `when=close` marker that the open line does not carry.
    #[test]
    fn close_line_is_marked_and_open_line_is_unchanged() {
        let render = |phase, timeout_units| {
            ConnParamsLine {
                conn: 1,
                role: LinkRole::Peripheral,
                params: ConnParams {
                    interval_units: 36,
                    latency: 0,
                    timeout_units,
                },
                phase,
            }
            .to_string()
        };
        assert_eq!(
            render(LinkPhase::Open, 42),
            "BLE_CONN_PARAMS conn=1 role=peripheral interval_ms=45.00 latency=0 timeout_ms=420"
        );
        assert_eq!(
            render(LinkPhase::Close, 400),
            "BLE_CONN_PARAMS conn=1 role=peripheral interval_ms=45.00 latency=0 timeout_ms=4000 \
             when=close"
        );
        // The honoured and the ignored close, side by side: the only
        // difference a reader has to see is the timeout.
        assert!(render(LinkPhase::Close, 42).ends_with("timeout_ms=420 when=close"));
    }
}
