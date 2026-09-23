//! Line shaping for the firmware's debug log, including the board-side
//! uptime stamp.
//!
//! # Why the board stamps its own lines
//!
//! Log lines are formatted into `LOG_RING` and drained later by
//! `debug_writer_task` in 64-byte USB packets on a 100 ms timeout loop.
//! A host-side capture therefore records *drain* time, not *emission*
//! time: deltas between two lines are compressed to zero inside one
//! drain burst and inflated to the loop period across bursts. A
//! measurement taken that way came out bimodal at 0 ms and exactly
//! 50 ms on two boards at once — the quantisation of the drain, not
//! anything either board did. Across two boards there is no common
//! time base at all.
//!
//! A stamp taken at format time, before the line enters the ring,
//! removes the drain from the measurement entirely.
//!
//! # The shape
//!
//! ```text
//! <prefix><body> t=<uptime_ms>\r\n
//! ```
//!
//! `t=` goes at the END and not in front of the `[TAG]`. Every existing
//! consumer anchors on the tag — `scripts/catch-reboot.sh` greps
//! `PANIC_COUNT] total`, `lnflash::verify` splits on `[FW_BUILD]`,
//! periculum's `fw_build_version_in` finds `[FW_BUILD]` — and a leading
//! stamp would break all of them at once. Appending also matches the
//! `EVENT key=val t=<ms>` convention the project already uses for
//! protocol events.
//!
//! # The last-`t=` rule
//!
//! A line may contain more than one `t=`. The boot-time replay of the
//! persistent tail wraps a line from the PREVIOUS boot, stamp and all,
//! inside a line of THIS boot:
//!
//! ```text
//! [INFO!] [PERSISTENT_LOG] [LORA] RX 41 bytes t=91422 t=137
//! ```
//!
//! Both stamps are true. The inner one is when the wrapped line was
//! emitted, last boot; the trailing one is when the replay line itself
//! was emitted, this boot. The stamp of a line is therefore always its
//! LAST `t=`, which is what [`parse_stamp`] reads. The same holds for a
//! `tracing` event whose own fields happen to include a `t`.
//!
//! # Which sink a line goes to
//!
//! Shaping is half of what makes a line readable; the other half is
//! whether it is emitted at all. [`facts`] holds the startup lines whose
//! *route* — the gated sink or the one that bypasses the gate — is a
//! property rather than a call-site detail, and states that route as data
//! so it can be asserted on the host.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod facts;

use core::fmt::Write;

/// Upper bound on the bytes ` t=<ms>` occupies: three for ` t=` and
/// twenty for the widest decimal `u64`.
pub const STAMP_MAX: usize = 3 + 20;

/// The trailing CRLF every line carries.
pub const TERMINATOR: usize = 2;

/// How many bytes of a `cap`-byte line buffer the prefix and body may
/// use.
///
/// The remainder is reserved so the stamp and the CRLF can never be the
/// part that gets truncated. The formatter has always dropped whatever
/// overran its buffer; with the stamp appended last, that silent drop
/// would land on the stamp — producing either a line that reads as
/// unstamped or, worse, one carrying half its digits. A stamp that is
/// sometimes wrong is worse than no stamp, so the body yields instead.
pub const fn body_limit(cap: usize) -> usize {
    cap.saturating_sub(STAMP_MAX + TERMINATOR)
}

/// A `core::fmt::Write` sink appending into `buf`, dropping whatever
/// does not fit.
///
/// Truncation rather than error is the firmware's long-standing
/// behaviour here: a log line is diagnostics, and a formatting failure
/// must never propagate into the code being diagnosed. This type makes
/// the boundary explicit so callers can cap the body at
/// [`body_limit`].
pub struct Sink<'a> {
    buf: &'a mut [u8],
    len: &'a mut usize,
}

impl<'a> Sink<'a> {
    /// Append into `buf`, continuing at `*len` and advancing it.
    pub fn new(buf: &'a mut [u8], len: &'a mut usize) -> Self {
        Self { buf, len }
    }
}

impl Write for Sink<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let remaining = self.buf.len().saturating_sub(*self.len);
        let to_copy = s.len().min(remaining);
        self.buf[*self.len..*self.len + to_copy].copy_from_slice(&s.as_bytes()[..to_copy]);
        *self.len += to_copy;
        Ok(())
    }
}

/// Append ` t=<stamp_ms>` and the CRLF, closing the line.
///
/// Call once, after the body, with the body capped at
/// `body_limit(buf.len())`. Returns the new length.
pub fn finish(buf: &mut [u8], mut len: usize, stamp_ms: u64) -> usize {
    let mut w = Sink::new(buf, &mut len);
    let _ = write!(w, " t={stamp_ms}");
    let _ = w.write_str("\r\n");
    len
}

/// Format one complete log line: `<prefix><args> t=<stamp_ms>\r\n`.
///
/// `stamp_ms` is milliseconds of board uptime, taken by the caller at
/// the moment of the log call.
pub fn format_line<'a>(
    buf: &'a mut [u8],
    prefix: &str,
    args: core::fmt::Arguments,
    stamp_ms: u64,
) -> &'a [u8] {
    let mut len = 0usize;
    {
        let limit = body_limit(buf.len());
        let mut w = Sink::new(&mut buf[..limit], &mut len);
        let _ = w.write_str(prefix);
        let _ = w.write_fmt(args);
    }
    let len = finish(buf, len, stamp_ms);
    &buf[..len]
}

/// Write a finite f64 as sign, integer part, and exactly six decimals
/// (1e-6 resolution) through integer formatting only.
///
/// This is the firmware log path's float rendering: the tracing
/// visitor's default `record_f64` forwards a value as `&dyn Debug`, and
/// that single f64-Debug vtable kept core's flt2dec apparatus (dragon,
/// grisu, their power tables, ~13 KiB) linked into both images even
/// though no event records an f64 today. Rounding is half-up on the
/// binary value; magnitudes above u64::MAX microunits saturate, which
/// is all a log line owes a float.
pub fn write_f64_micro(w: &mut impl core::fmt::Write, value: f64) -> core::fmt::Result {
    let neg = value.is_sign_negative();
    let mag = if neg { -value } else { value };
    // core has no f64::round; +0.5-then-truncate rounds half-up for the
    // non-negative finite values left after the sign split.
    let micros = (mag * 1e6 + 0.5) as u64;
    if neg {
        w.write_char('-')?;
    }
    write!(w, "{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// The body of the firmware's `LINK_REFUSED` line: the link-table cap
/// refusal (#388), rendered from the core's `NodeEvent::LinkRefused`
/// because the boards build leviculum-core without `tracing` and the
/// core's own refusal line is compiled out there.
///
/// Key set matches that line (`reason=budget links= max= dest=`), so a
/// capped node's refusals grep alike on a board's CDC capture and in an
/// lnsd log. `dest` is shortened to its first 4 bytes (8 hex chars) —
/// enough to correlate against the initiator's own logs without spending
/// the line budget on a full hash.
pub struct LinkRefusedBody {
    /// Live link-table entries at refusal time.
    pub links: usize,
    /// The configured cap that was hit.
    pub max: usize,
    /// The destination the refused link was for.
    pub dest: [u8; 16],
}

impl core::fmt::Display for LinkRefusedBody {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "reason=budget links={} max={} dest=",
            self.links, self.max
        )?;
        for byte in &self.dest[..4] {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The body of the firmware's `ANNOUNCE_LEARNED_NOT_RELAYED` line, rendered
/// from the core's `NodeEvent::AnnounceLearnedNotRelayed` for the same reason
/// [`LinkRefusedBody`] is: the boards build leviculum-core without `tracing`
/// and the core's own line is compiled out there.
///
/// **The line does not report a loss.** An announce reached this board,
/// was validated, and updated its path table; the board then had no route to
/// pass it on. `closed` says why the announce table opened no rebroadcast and
/// `discovery` whether anyone had a path request open for the destination.
/// A reader who greps this for packet loss will mis-read it — the packet
/// arrived, and the path it carried is installed.
///
/// `closed` and `discovery` are the core's own scalars
/// (`AnnounceTableClosed::as_str`, `DiscoveryWindow::as_str`), passed through
/// as `&'static str` so this crate keeps no dependencies. `dest` is shortened
/// to its first 4 bytes, like [`LinkRefusedBody`]'s.
pub struct AnnounceLearnedNotRelayedBody {
    /// Why the announce table opened no rebroadcast.
    pub closed: &'static str,
    /// The state of the destination's discovery path request.
    pub discovery: &'static str,
    /// The destination the announce was for.
    pub dest: [u8; 16],
}

impl core::fmt::Display for AnnounceLearnedNotRelayedBody {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "closed={} discovery={} dest=",
            self.closed, self.discovery
        )?;
        for byte in &self.dest[..4] {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The body of the firmware's `PKT_RELAY` line, rendered from the core's
/// `NodeEvent::RelayDecided` (Codeberg #346).
///
/// What this board did with ONE packet a neighbour addressed to it for relay:
///
/// ```text
/// PKT_RELAY outcome=<o> ph=<16 hex> dst=<8 hex> hops=<n> iface_out=<n|none>
/// ```
///
/// Off the boards the same decision is already readable as the journey events
/// `PKT_FORWARD`, `PKT_DROP` and `DEDUP_DROP`. On a board none of them exists
/// — the firmware builds leviculum-core without `tracing` — and the periodic
/// `[TRANSPORT]` counter line can only say how many, never which. This line
/// is the "which".
///
/// `ph` is the FULL 8-byte journey correlator, rendered exactly as
/// `leviculum-core`'s `ph=` renders it, so a board line and a peer's `lnsd`
/// log stitch on one id. `dest` is shortened to 4 bytes like
/// [`LinkRefusedBody`]'s — a prefix of the 16-byte `dst=` the host events
/// carry, so a grep for the short form still finds both.
///
/// **Absence of a line is itself a reading.** A packet nobody addressed to
/// this board produces none: the overheard path is counter-only by design, so
/// "no `PKT_RELAY` for a packet a peer says it sent" means the board either
/// never heard it or was never named as its next hop — not that the relay
/// silently swallowed it.
pub struct RelayDecidedBody {
    /// The outcome scalar, the core's own (`RelayOutcome::as_str`), passed
    /// through as `&'static str` so this crate keeps no dependencies.
    pub outcome: &'static str,
    /// The journey correlator: the first 8 bytes of the packet hash.
    pub ph: [u8; 8],
    /// The destination the packet was for.
    pub dest: [u8; 16],
    /// The packet's hop count as it arrived.
    pub hops: u8,
    /// The interface it was handed to, or `None` when it reached none.
    pub iface_out: Option<usize>,
}

impl core::fmt::Display for RelayDecidedBody {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "outcome={} ph=", self.outcome)?;
        for byte in &self.ph {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(" dst=")?;
        for byte in &self.dest[..4] {
            write!(f, "{byte:02x}")?;
        }
        write!(f, " hops={}", self.hops)?;
        // A fixed key set: `none` rather than an omitted key, so a consumer
        // that splits on `=` never has to tell a missing field from a
        // truncated line.
        match self.iface_out {
            Some(idx) => write!(f, " iface_out={idx}"),
            None => f.write_str(" iface_out=none"),
        }
    }
}

/// The uptime stamp of a captured line: its LAST `t=` field.
///
/// `None` for a line that carries none — every line the current
/// firmware emits at runtime carries one, so a `None` on a capture is
/// itself a finding (old firmware, or a line torn by the reader).
///
/// Host-side consumers should use this rather than a hand-rolled
/// `find("t=")`, which would pick up the replayed inner stamp of a
/// `[PERSISTENT_LOG]` line.
pub fn parse_stamp(line: &str) -> Option<u64> {
    line.trim_end_matches(['\r', '\n'])
        .rsplit(' ')
        .find_map(|field| field.strip_prefix("t="))
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;

    fn line(prefix: &str, args: core::fmt::Arguments, stamp: u64) -> String {
        let mut buf = [0u8; 1024];
        let bytes = format_line(&mut buf, prefix, args, stamp);
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// The integer micro-formatter replaced the default tracing
    /// `record_f64` path (f64 as `{:?}`). The float formatter's own
    /// six-decimal rendering is the expected value: every output must
    /// match `format!("{:.6}")` byte for byte, across signs, magnitudes,
    /// carry-through rounding, negative zero, and saturation.
    #[test]
    fn f64_micro_matches_the_float_formatting_it_replaced() {
        let mut cases: std::vec::Vec<f64> = std::vec![
            0.0,
            -0.0,
            1.5,
            -1.5,
            0.000001,
            -0.000001,
            0.9999996,
            -0.9999996,
            1234.567891,
            53.075161,
            -8.807771,
        ];
        for i in 0..100_000u32 {
            cases.push(f64::from(i) * 1e-6 + 3.3e-7);
        }
        for v in cases {
            let mut buf = [0u8; 64];
            let mut len = 0usize;
            let mut w = Sink::new(&mut buf, &mut len);
            write_f64_micro(&mut w, v).unwrap();
            let got = core::str::from_utf8(&buf[..len]).unwrap();
            assert_eq!(got, std::format!("{v:.6}"), "value {v:?}");
        }
    }

    /// Out-of-range magnitudes saturate instead of wrapping: the exact
    /// digits stop mattering above u64::MAX microunits, the sign and
    /// "very large" must survive.
    #[test]
    fn f64_micro_saturates_on_overflow() {
        let mut buf = [0u8; 64];
        let mut len = 0usize;
        let mut w = Sink::new(&mut buf, &mut len);
        write_f64_micro(&mut w, 1e30).unwrap();
        let got = core::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(
            got,
            std::format!("{}.{:06}", u64::MAX / 1_000_000, u64::MAX % 1_000_000)
        );
    }

    /// The swallowed-announce line, byte for byte, in the shape the field
    /// capture has to be readable in: the board learned a path from a
    /// PATH_RESPONSE announce past its discovery window and had nowhere to
    /// send it. 91 bytes on the wire including the stamp and the CRLF.
    #[test]
    fn the_announce_learned_not_relayed_line_has_the_shape_the_capture_greps() {
        let mut dest = [0u8; 16];
        dest[..4].copy_from_slice(&[0xbb, 0x12, 0x4c, 0x3b]);
        let body = AnnounceLearnedNotRelayedBody {
            closed: "path_response",
            discovery: "expired",
            dest,
        };
        assert_eq!(
            line(
                "ANNOUNCE_LEARNED_NOT_RELAYED ",
                format_args!("{body}"),
                131002
            ),
            concat!(
                "ANNOUNCE_LEARNED_NOT_RELAYED closed=path_response ",
                "discovery=expired dest=bb124c3b t=131002\r\n"
            )
        );
        // The per-occurrence cost on the debug CDC, stated as a number
        // rather than as an estimate: the widest scalar combination the
        // core can emit is `path_response`/`expired`, at the widest stamp.
        let widest = AnnounceLearnedNotRelayedBody {
            closed: "path_response",
            discovery: "expired",
            dest,
        };
        assert_eq!(
            line(
                "ANNOUNCE_LEARNED_NOT_RELAYED ",
                format_args!("{widest}"),
                u64::MAX
            )
            .len(),
            106
        );
    }

    #[test]
    fn a_whole_line_has_the_shape_the_consumers_expect() {
        assert_eq!(
            line(
                "[INFO] ",
                format_args!("RX {} bytes rssi={}", 41, -97),
                1234
            ),
            "[INFO] RX 41 bytes rssi=-97 t=1234\r\n"
        );
    }

    /// The whole refusal line, byte for byte, as the #388 board proof
    /// greps it: prefix, the numbers, the 8-hex-char destination, the
    /// stamp. The firmware crate runs no host tests, so this is where the
    /// rendering is held to the shape the capture consumer expects.
    #[test]
    fn the_link_refused_line_has_the_shape_the_capture_greps() {
        let mut dest = [0u8; 16];
        dest[..4].copy_from_slice(&[0xab, 0x01, 0xcd, 0x23]);
        let body = LinkRefusedBody {
            links: 5,
            max: 5,
            dest,
        };
        assert_eq!(
            line("LINK_REFUSED ", format_args!("{body}"), 42),
            "LINK_REFUSED reason=budget links=5 max=5 dest=ab01cd23 t=42\r\n"
        );
    }

    #[test]
    fn a_forwarded_relay_names_the_packet_and_the_interface_it_left_on() {
        let mut dest = [0u8; 16];
        dest[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let body = RelayDecidedBody {
            outcome: "forwarded",
            ph: [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef],
            dest,
            hops: 2,
            iface_out: Some(0),
        };
        assert_eq!(
            line("PKT_RELAY ", format_args!("{body}"), 1234),
            "PKT_RELAY outcome=forwarded ph=0123456789abcdef dst=deadbeef hops=2 \
iface_out=0 t=1234\r\n"
        );
    }

    #[test]
    fn a_relay_that_reached_no_interface_says_none_rather_than_dropping_the_key() {
        // The failure this pins: a dropped key turns "reached no interface"
        // into "the line was cut", and the two read the same in a capture.
        let body = RelayDecidedBody {
            outcome: "no-path",
            ph: [0xaa; 8],
            dest: [0x11; 16],
            hops: 1,
            iface_out: None,
        };
        let rendered = line("PKT_RELAY ", format_args!("{body}"), 7);
        assert_eq!(
            rendered,
            "PKT_RELAY outcome=no-path ph=aaaaaaaaaaaaaaaa dst=11111111 hops=1 \
iface_out=none t=7\r\n"
        );
        assert_eq!(rendered.matches('=').count(), 6);
    }

    #[test]
    fn the_widest_relay_line_fits_the_firmware_log_buffer() {
        // The widest body the four outcomes can produce, at the widest hop
        // count and a two-digit interface index. It must leave room for the
        // stamp inside one 256-byte log buffer, or `format_line` starts
        // trading the body against the `t=` the capture is ordered by.
        let body = RelayDecidedBody {
            outcome: "forward-max-hops",
            ph: [0xff; 8],
            dest: [0xff; 16],
            hops: u8::MAX,
            iface_out: Some(99),
        };
        let rendered = line("PKT_RELAY ", format_args!("{body}"), u64::MAX);
        assert!(
            rendered.len() < 128,
            "{} bytes: {rendered:?}",
            rendered.len()
        );
        assert_eq!(parse_stamp(&rendered), Some(u64::MAX));
    }

    #[test]
    fn the_empty_body_still_closes_the_line() {
        // `[T114_SX_TIMEOUT]` logs with an empty body; the prefix's own
        // trailing space then meets the stamp's leading one.
        assert_eq!(
            line("[T114_SX_TIMEOUT] ", format_args!(""), 7),
            "[T114_SX_TIMEOUT]  t=7\r\n"
        );
    }

    #[test]
    fn the_stamp_is_rendered_verbatim_at_both_ends_of_the_range() {
        assert_eq!(parse_stamp(&line("[X] ", format_args!("a"), 0)), Some(0));
        assert_eq!(
            parse_stamp(&line("[X] ", format_args!("a"), u64::MAX)),
            Some(u64::MAX)
        );
    }

    #[test]
    fn a_body_that_overruns_the_buffer_loses_the_body_not_the_stamp() {
        // The failure this reserves against: the body eats the buffer,
        // the stamp is truncated to a few digits, and the line reads as
        // a plausible-but-wrong measurement.
        let mut buf = [0u8; 64];
        let long = "0123456789".repeat(20);
        let bytes = format_line(&mut buf, "[X] ", format_args!("{long}"), 4_294_967_295);
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.ends_with(" t=4294967295\r\n"), "got {text:?}");
        assert_eq!(parse_stamp(text), Some(4_294_967_295));
        assert!(bytes.len() <= 64);
    }

    #[test]
    fn body_limit_reserves_the_stamp_and_the_terminator() {
        assert_eq!(body_limit(1024), 1024 - 25);
        // A buffer too small for a stamp yields a zero body rather than
        // an underflow.
        assert_eq!(body_limit(4), 0);
    }

    #[test]
    fn the_stamp_of_a_replayed_line_is_the_outer_one() {
        // A [PERSISTENT_LOG] replay carries last boot's stamp inside
        // this boot's. The line's own stamp is the trailing one.
        let replayed = "[INFO!] [PERSISTENT_LOG] [LORA] RX 41 bytes t=91422 t=137\r\n";
        assert_eq!(parse_stamp(replayed), Some(137));
    }

    #[test]
    fn parse_stamp_declines_what_is_not_a_stamp() {
        assert_eq!(parse_stamp("[LORA] RX 41 bytes"), None);
        assert_eq!(parse_stamp("[LORA] t=abc"), None);
        // A `t=` glued to the end of another token is a different field,
        // not this one.
        assert_eq!(parse_stamp("[LORA] rtt=5"), None);
        assert_eq!(parse_stamp("[LORA] t="), None);
    }

    #[test]
    fn lines_emitted_in_order_carry_non_decreasing_stamps() {
        // The clock is monotonic (`embassy_time::Instant`), and the
        // formatter renders what it is given: so an ordered sequence of
        // stamps survives formatting as an ordered sequence of lines.
        let stamps = [0u64, 1, 1, 999, 1_000, 86_400_000];
        let mut prev = 0u64;
        for s in stamps {
            let rendered = parse_stamp(&line("[LORA] ", format_args!("op=rx_success"), s))
                .expect("every emitted line carries a stamp");
            assert_eq!(rendered, s);
            assert!(rendered >= prev, "{rendered} < {prev}");
            prev = rendered;
        }
    }

    #[test]
    fn a_deaf_window_is_the_arithmetic_the_stamps_were_added_for() {
        // Two consecutive rx_success lines. The radio was not listening
        // between the end of the first reception and the start of the
        // second: t2 - duration2 - t1.
        let first = "[T114_LORA_LOOP] op=rx_success duration_ms=120 t=1000\r\n";
        let second = "[T114_LORA_LOOP] op=rx_success duration_ms=80 t=1500\r\n";
        let t1 = parse_stamp(first).unwrap();
        let t2 = parse_stamp(second).unwrap();
        let d2: u64 = 80;
        assert_eq!(t2 - d2 - t1, 420);
    }
}
