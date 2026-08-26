//! The startup facts a board must be able to state about itself, and the
//! sink each of them is written to.
//!
//! # Why routing is a property and not a call-site detail
//!
//! The firmware has two log sinks. `log_fmt` is gated on
//! `RUNTIME_DRAIN_OPEN` and silently drops everything until a reader has
//! attached to the debug port; `log_fmt_critical` bypasses that gate. The
//! difference is invisible at a call site — the two spellings differ by one
//! word — and it decides whether a line can ever be read on a board that
//! booted before anyone was listening.
//!
//! Which is most boards. Measured at the bench on a reflashed T114, with a
//! reader already waiting for the port to come back:
//!
//! ```text
//! [INFO!] [STG] lora-init t=191
//! [INFO!] [STG] main-loop t=1886
//! [SX_REG] rxgain_before=0x94 rxgain_after=0x96 txmod=0x04 t=1921
//! [LOG_GATE] opened, dropped 20 runtime lines pre-attach t=2581
//! ```
//!
//! The radio task ran from `t=191` and the gate opened at `t=2581`: twenty
//! lines emitted in between were dropped, and among them were the only two
//! lines that say what the radio was set to and under what lawful duty-cycle
//! cap it was transmitting. Neither is obtainable from a running board any
//! other way — the applied configuration lives in the radio's registers and
//! the derived cap lives in the airtime tracker, and nothing reads either
//! back out. For a *regulatory* limit that is the wrong property to have:
//! the question "under what cap is this board transmitting?" has to be
//! answerable at the bench, not reconstructable from the source.
//!
//! So the two are emitted here, through [`LineSink`], with their route
//! stated as data. They are one-shot startup facts and not a stream: the
//! cost is two lines per boot. Anything that repeats belongs on the gated
//! sink, where it cannot lap the ring before a reader arrives.
//!
//! The functions take a sink rather than calling the firmware's logger, so
//! the routing is assertable on the host: `leviculum-nrf` cross-compiles to
//! thumbv7em and runs no host tests, and reading the source to check which
//! function a call site names proves nothing about what the board emits.

use core::fmt::Arguments;

/// Which of the firmware's two log sinks a line is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Bypasses the runtime drain gate: reaches the host even when the line
    /// was emitted before anything attached to the debug port.
    Critical,
    /// Dropped while the runtime drain gate is closed.
    Gated,
}

/// The firmware's debug log, as the facts below see it.
///
/// The firmware's implementation maps [`Route::Critical`] onto
/// `log_fmt_critical` and [`Route::Gated`] onto `log_fmt`; a test's
/// implementation records the route it was handed.
pub trait LineSink {
    /// Emit one line: the prefix, the body, and the route it goes out on.
    fn line(&mut self, route: Route, prefix: &str, args: Arguments);
}

/// The radio settings the modem was actually programmed with.
///
/// Human-readable throughout — `bw_hz` and `cr_denom` rather than the
/// SX1262 register codes — because the reader of this line is an operator
/// answering "what is this board doing", not the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveRadioConfig {
    pub freq_hz: u32,
    pub sf: u8,
    pub bw_hz: u32,
    pub cr_denom: u8,
    pub txp_dbm: i8,
    pub csma: bool,
}

/// `[LORA] active config: …` — what the radio is set to, emitted after
/// every successful `configure_lora`.
///
/// Critical: it is emitted once at boot and once per runtime
/// reconfiguration, and a board reconfigured in the field has to be able to
/// say what it was reconfigured to.
pub fn active_radio_config<S: LineSink>(sink: &mut S, c: &ActiveRadioConfig) {
    sink.line(
        Route::Critical,
        "[LORA] ",
        format_args!(
            "active config: freq={} sf={} bw={} cr={} txp={} csma={}",
            c.freq_hz, c.sf, c.bw_hz, c.cr_denom, c.txp_dbm, c.csma
        ),
    );
}

/// `[LORA_AIRTIME_LOCK] lawful default …` — the regulatory duty-cycle cap
/// the firmware derived from its own TX frequency because the host sent no
/// explicit `lt_alock`.
///
/// Critical for the same reason, and more sharply: this is the number a
/// compliance question is about. `lt_alock` is the RNode `CMD_LT_ALOCK`
/// u16 encoding (percent × 100), so `100` is 1 % and `0` is unlimited.
pub fn lawful_airtime_default<S: LineSink>(sink: &mut S, freq_hz: u32, lt_alock: u16) {
    sink.line(
        Route::Critical,
        "[LORA_AIRTIME_LOCK] ",
        format_args!("lawful default freq={freq_hz} lt_alock={lt_alock}"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;
    use std::vec::Vec;

    /// Records what the firmware's logger would have been handed. The line
    /// is rendered through the real [`crate::format_line`], so the text
    /// asserted below is the text that reaches the ring buffer.
    #[derive(Default)]
    struct Recorder {
        lines: Vec<(Route, String)>,
    }

    impl LineSink for Recorder {
        fn line(&mut self, route: Route, prefix: &str, args: Arguments) {
            let mut buf = [0u8; 1024];
            let bytes = crate::format_line(&mut buf, prefix, args, 191);
            self.lines
                .push((route, String::from_utf8_lossy(bytes).into_owned()));
        }
    }

    fn eu_medium() -> ActiveRadioConfig {
        ActiveRadioConfig {
            freq_hz: 869_463_000,
            sf: 8,
            bw_hz: 125_000,
            cr_denom: 5,
            txp_dbm: 22,
            csma: true,
        }
    }

    #[test]
    fn the_applied_configuration_goes_out_on_the_critical_sink() {
        let mut sink = Recorder::default();
        active_radio_config(&mut sink, &eu_medium());
        assert_eq!(
            sink.lines,
            [(
                Route::Critical,
                String::from(
                    "[LORA] active config: freq=869463000 sf=8 bw=125000 cr=5 txp=22 \
                     csma=true t=191\r\n"
                )
            )]
        );
    }

    #[test]
    fn the_lawful_cap_goes_out_on_the_critical_sink() {
        let mut sink = Recorder::default();
        // 869.463 MHz falls in ERC 70-03 h1.7, 10 % duty cycle; the
        // `CMD_LT_ALOCK` encoding of 10 % is 1000.
        lawful_airtime_default(&mut sink, 869_463_000, 1000);
        assert_eq!(
            sink.lines,
            [(
                Route::Critical,
                String::from(
                    "[LORA_AIRTIME_LOCK] lawful default freq=869463000 lt_alock=1000 t=191\r\n"
                )
            )]
        );
    }

    /// The reason these two are on the critical sink at all: a board that
    /// boots before a reader attaches drops everything gated, and the two
    /// facts an operator needs must survive that.
    #[test]
    fn a_pre_attach_boot_keeps_both_facts_and_drops_the_gated_ones() {
        struct PreAttach {
            kept: Vec<String>,
            dropped: usize,
        }
        impl LineSink for PreAttach {
            fn line(&mut self, route: Route, prefix: &str, args: Arguments) {
                if route == Route::Gated {
                    self.dropped += 1;
                    return;
                }
                let mut buf = [0u8; 1024];
                let bytes = crate::format_line(&mut buf, prefix, args, 191);
                self.kept.push(String::from_utf8_lossy(bytes).into_owned());
            }
        }

        let mut sink = PreAttach {
            kept: Vec::new(),
            dropped: 0,
        };
        // A routine runtime line, for contrast: it is gated and is lost.
        sink.line(Route::Gated, "[LORA] ", format_args!("RX 41 bytes"));
        active_radio_config(&mut sink, &eu_medium());
        lawful_airtime_default(&mut sink, 869_463_000, 1000);

        assert_eq!(sink.dropped, 1);
        assert_eq!(sink.kept.len(), 2, "kept: {:?}", sink.kept);
        assert!(sink.kept[0].starts_with("[LORA] active config: "));
        assert!(sink.kept[1].starts_with("[LORA_AIRTIME_LOCK] lawful default "));
    }

    /// An out-of-band frequency derives no lawful cap, and the line has to
    /// say `0` rather than not be emitted: "no cap was derived" is the
    /// answer an operator most needs and least expects.
    #[test]
    fn an_underived_cap_is_still_stated() {
        let mut sink = Recorder::default();
        lawful_airtime_default(&mut sink, 915_000_000, 0);
        assert_eq!(sink.lines[0].0, Route::Critical);
        assert!(
            sink.lines[0]
                .1
                .contains("lawful default freq=915000000 lt_alock=0"),
            "got {:?}",
            sink.lines[0].1
        );
    }
}
