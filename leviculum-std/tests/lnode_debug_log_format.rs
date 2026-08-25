//! Codeberg #65 instrumentation contract: the shape of the LNode debug lines
//! the host side greps.
//!
//! The firmware emits two line shapes on the CDC-ACM debug console:
//!
//! ```text
//! [INFO!] [PANIC_COUNT] total=<u32> t=<ms>                  (boot banner, once per boot)
//! [HEAP] used=<n> free=<n> watermark=<n> size=<n> t=<ms>    (every 30 s)
//! [SX_REG] rxgain_before=0xNN rxgain_after=0xNN txmod=0xNN  (once, end of init_radio)
//! [SX_REG_IQ] iq_before=0xNN iq_after=0xNN txmod=0xNN       (once, first SetPacketParams)
//! ```
//!
//! The trailing ` t=<ms>` is board uptime at the moment the line was
//! formatted, appended to EVERY runtime line since the drain-latency audit
//! (#344). It is deliberately at the END: every consumer below anchors on a
//! `[TAG]`, and a leading stamp would break all of them at once. The parsers
//! here therefore have to keep working with an unknown-key field appended —
//! which is exactly what `stamp_does_not_disturb_the_existing_parsers` asserts.
//!
//! Two host-side consumers grep them: `scripts/catch-reboot.sh`, which reports
//! the cause of a reboot caught under sustained LoRa load, and the ad-hoc heap
//! analysis of a debug capture (Codeberg #50 left the peak-load stack question
//! open, so `[HEAP]` is still read by hand). If the firmware changes the shape,
//! both silently grep nothing. These tests fail first.
//!
//! Migrated here from `reticulum-integ/tests/lnode_instrumentation.rs` when
//! that crate was retired. The hardware half of that file — a live listen on
//! every attached LNode's debug port asserting the telemetry really flows —
//! did NOT come with it: periculum captures the debug port but has no step
//! class that asserts on the capture, so there is nowhere to express it. See
//! the retirement ledger.
//!
//! This lives in leviculum-std because the firmware crate cross-compiles to
//! thumbv7em and cannot run host tests, and leviculum-std is the crate that
//! owns host-side communication with a board.

use std::path::{Path, PathBuf};

/// Extract the persistent panic counter from a debug-capture line.
fn parse_panic_count(line: &str) -> Option<u32> {
    let idx = line.find("[PANIC_COUNT] total=")?;
    let rest = &line[idx + "[PANIC_COUNT] total=".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Extract (used, free, watermark, size) from a `[HEAP]` telemetry line.
fn parse_heap_line(line: &str) -> Option<(u64, u64, u64, u64)> {
    let idx = line.find("[HEAP] ")?;
    let rest = &line[idx + "[HEAP] ".len()..];
    let mut used = None;
    let mut free = None;
    let mut watermark = None;
    let mut size = None;
    for token in rest.split_whitespace() {
        let (key, value) = token.split_once('=')?;
        let value: u64 = value.parse().ok()?;
        match key {
            "used" => used = Some(value),
            "free" => free = Some(value),
            "watermark" => watermark = Some(value),
            "size" => size = Some(value),
            _ => {}
        }
    }
    Some((used?, free?, watermark?, size?))
}

#[test]
fn panic_count_banner_line_parses() {
    // Real shape as emitted via log_critical! on boot.
    assert_eq!(parse_panic_count("[INFO!] [PANIC_COUNT] total=0"), Some(0));
    assert_eq!(parse_panic_count("[PANIC_COUNT] total=17"), Some(17));
    // Prefixed by capture-side timestamps or banner noise still parses.
    assert_eq!(
        parse_panic_count("2026-06-12T20:46:01Z [INFO!] [PANIC_COUNT] total=3 trailing"),
        Some(3)
    );
    assert_eq!(parse_panic_count("[INFO!] leviculum T114 booting"), None);
    assert_eq!(parse_panic_count("[PANIC_COUNT] total="), None);
}

#[test]
fn heap_telemetry_line_parses() {
    // Real line captured from the flashed T114 (2026-06-12).
    let line = "[HEAP] used=52376 free=13156 watermark=52376 size=65536";
    assert_eq!(parse_heap_line(line), Some((52376, 13156, 52376, 65536)));
    // Real line captured from the flashed Pocket V2.
    let line = "[HEAP] used=50016 free=15516 watermark=50016 size=65536";
    assert_eq!(parse_heap_line(line), Some((50016, 15516, 50016, 65536)));
    // Persistent-tail replay prefixes the tag chain; still parses.
    let line = "[INFO!] [PERSISTENT_LOG] [HEAP] used=1 free=2 watermark=3 size=4";
    assert_eq!(parse_heap_line(line), Some((1, 2, 3, 4)));
    assert_eq!(parse_heap_line("[HEAP] used=1 free=2"), None);
    assert_eq!(parse_heap_line("[DIAG_MEM] stack_free=9000"), None);
}

/// Extract the hex-valued fields of an `[SX_REG]`-family register read-back.
///
/// Returns the fields in the order the line carries them. Shared by both
/// lines: they differ in tag and field names, not in shape, and a consumer
/// that wanted one of them by name would have to parse `key=0xNN` anyway.
fn parse_reg_line(line: &str, tag: &str, keys: &[&str]) -> Option<Vec<u8>> {
    let idx = line.find(tag)?;
    let rest = &line[idx + tag.len()..];
    let mut found: Vec<Option<u8>> = keys.iter().map(|_| None).collect();
    for token in rest.split_whitespace() {
        let (key, value) = token.split_once('=')?;
        let Some(pos) = keys.iter().position(|k| *k == key) else {
            continue; // the ` t=` stamp, and anything appended after it
        };
        found[pos] = u8::from_str_radix(value.strip_prefix("0x")?, 16).ok();
    }
    found.into_iter().collect()
}

const SX_REG_KEYS: [&str; 3] = ["rxgain_before", "rxgain_after", "txmod"];
const SX_REG_IQ_KEYS: [&str; 3] = ["iq_before", "iq_after", "txmod"];

/// The two register read-back lines parse, stamp and replay-wrapper included.
///
/// These are the only evidence a capture can carry that `35fdd87`'s two
/// register writes took effect: an unwritten register and a written one look
/// identical in every other line the firmware emits. The `before` field is
/// what makes each line a measurement — `after` alone would say a register
/// holds a value, not that we put it there — so a parser that dropped it
/// would silently turn the measurement back into an assertion.
#[test]
fn register_readback_lines_parse() {
    assert_eq!(
        parse_reg_line(
            "[SX_REG] rxgain_before=0x94 rxgain_after=0x96 txmod=0x0D t=412",
            "[SX_REG] ",
            &SX_REG_KEYS
        ),
        Some(vec![0x94, 0x96, 0x0D])
    );
    assert_eq!(
        parse_reg_line(
            "[SX_REG_IQ] iq_before=0x0D iq_after=0x0D txmod=0x0D t=511",
            "[SX_REG_IQ] ",
            &SX_REG_IQ_KEYS
        ),
        Some(vec![0x0D, 0x0D, 0x0D])
    );
    // The line is emitted through `log_fmt_critical`, so a board that crashed
    // replays it wrapped on the next boot. Still parses.
    assert_eq!(
        parse_reg_line(
            "[INFO!] [PERSISTENT_LOG] [SX_REG] rxgain_before=0x96 rxgain_after=0x96 txmod=0x0D t=412 t=7",
            "[SX_REG] ",
            &SX_REG_KEYS
        ),
        Some(vec![0x96, 0x96, 0x0D])
    );
    // A field missing is a parse failure, not a zero. A zero would read as a
    // register that answered 0x00.
    assert_eq!(
        parse_reg_line(
            "[SX_REG] rxgain_before=0x94 txmod=0x0D t=412",
            "[SX_REG] ",
            &SX_REG_KEYS
        ),
        None
    );
    // The two tags do not answer for each other.
    assert_eq!(
        parse_reg_line(
            "[SX_REG] rxgain_before=0x94 rxgain_after=0x96 txmod=0x0D",
            "[SX_REG_IQ] ",
            &SX_REG_IQ_KEYS
        ),
        None
    );
}

/// The firmware still emits what the parsers above expect, and — the part
/// that matters — each `after` value is still a second read of the register
/// rather than the value we sent.
///
/// The shaping lives in `leviculum_core::sx126x`'s two `Display` impls and the
/// read-write-read brackets in `probe_rx_init` / `apply_iq_polarity`, which
/// have their own host tests (`sx126x::probe_tests`). What is pinned here is
/// the seam those tests cannot see: that the firmware actually calls them, and
/// with the tag the host greps.
#[test]
fn the_firmware_still_emits_the_register_readback() {
    let sx = nrf_source("sx1262.rs");
    for (tag, call) in [
        ("\"[SX_REG] \"", "sx126x::probe_rx_init(self)"),
        ("\"[SX_REG_IQ] \"", "sx126x::apply_iq_polarity(self, false,"),
    ] {
        assert!(
            sx.contains(tag),
            "leviculum-nrf/src/sx1262.rs no longer emits the {tag} tag"
        );
        assert!(
            sx.contains(call),
            "leviculum-nrf/src/sx1262.rs no longer calls {call}, so the line it \
             logs is no longer the core probe's bracketed read-back"
        );
    }
    // Boot-critical, both of them: `init_radio` and the first
    // `SetPacketParams` run long before DTR-assert opens the runtime drain, so
    // a `log_fmt` here would be counted in RUNTIME_DROPPED and thrown away.
    for tag in ["[SX_REG] ", "[SX_REG_IQ] "] {
        let idx = sx
            .find(tag)
            .unwrap_or_else(|| panic!("leviculum-nrf/src/sx1262.rs: {tag} tag gone"));
        let call_start = sx[..idx].rfind("crate::log::").unwrap_or_else(|| {
            panic!("leviculum-nrf/src/sx1262.rs: {tag} is not logged through crate::log")
        });
        assert!(
            sx[call_start..idx].contains("log_fmt_critical"),
            "leviculum-nrf/src/sx1262.rs logs {tag} through the runtime-gated \
             log_fmt; it is emitted before DTR-assert and would be dropped"
        );
    }
    // The field names and their order are the parsers' contract, and they live
    // in core's Display impls.
    let core_src = {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("leviculum-core/src/sx126x.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };
    assert!(
        core_src.contains("rxgain_before=0x{:02X} rxgain_after=0x{:02X} txmod=0x{:02X}"),
        "the [SX_REG] field order or spelling changed in leviculum-core/src/sx126x.rs"
    );
    assert!(
        core_src.contains("iq_before=0x{:02X} iq_after=0x{:02X} txmod=0x{:02X}"),
        "the [SX_REG_IQ] field order or spelling changed in leviculum-core/src/sx126x.rs"
    );
}

fn nrf_source(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("leviculum-nrf/src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The parsers above are only worth anything if they still describe what the
/// firmware writes. Pin the emitting format strings in the firmware source, so
/// a rename there fails here rather than in a grep that quietly returns
/// nothing months later.
#[test]
fn the_firmware_still_emits_the_shapes_these_parsers_expect() {
    let lib = nrf_source("lib.rs");
    assert!(
        lib.contains(r#""[HEAP] ""#),
        "leviculum-nrf/src/lib.rs no longer emits the `[HEAP] ` tag"
    );
    assert!(
        lib.contains("used={used} free={free} watermark={watermark} size={HEAP_SIZE}"),
        "the [HEAP] field order or spelling changed in leviculum-nrf/src/lib.rs"
    );
    // The banner format string lives in the shared helper (48f34f6 moved it
    // out of the BSP bins); each bin must still invoke it at boot.
    assert!(
        lib.contains("[PANIC_COUNT] total="),
        "leviculum-nrf/src/lib.rs no longer emits the [PANIC_COUNT] banner"
    );
    for bin in ["bin/t114.rs", "bin/rak4631.rs"] {
        assert!(
            nrf_source(bin).contains("log_panic_count()"),
            "leviculum-nrf/src/{bin} no longer emits the [PANIC_COUNT] banner at boot"
        );
    }
}

/// The uptime stamp of a captured line: its LAST `t=` field.
///
/// The last, not the first: a `[PERSISTENT_LOG]` replay wraps a line from the
/// previous boot — its stamp included — inside a line of this boot. The
/// firmware-side definition of this rule lives in `leviculum-log-line`; this
/// is the host-side copy, and the two are pinned against each other by
/// `the_firmware_stamps_every_line_with_board_uptime` below.
fn parse_stamp(line: &str) -> Option<u64> {
    line.trim_end_matches(['\r', '\n'])
        .rsplit(' ')
        .find_map(|field| field.strip_prefix("t="))
        .and_then(|v| v.parse().ok())
}

#[test]
fn uptime_stamp_parses_off_the_end_of_a_line() {
    assert_eq!(
        parse_stamp("[HEAP] used=52376 free=13156 watermark=52376 size=65536 t=91422"),
        Some(91422)
    );
    // A replayed line carries two. The line's own stamp is the outer one.
    assert_eq!(
        parse_stamp("[INFO!] [PERSISTENT_LOG] [LORA] RX 41 bytes t=91422 t=137"),
        Some(137)
    );
    assert_eq!(parse_stamp("[LORA] RX 41 bytes"), None);
    assert_eq!(parse_stamp("[LORA] rtt=5"), None);
}

#[test]
fn stamp_does_not_disturb_the_existing_parsers() {
    // The whole risk of appending a field is that a consumer keyed on the
    // rest of the line stops seeing it. Both parsers above, on real stamped
    // lines, must return exactly what they returned before the stamp existed.
    assert_eq!(
        parse_panic_count("[INFO!] [PANIC_COUNT] total=3 t=137"),
        Some(3)
    );
    assert_eq!(
        parse_heap_line("[HEAP] used=52376 free=13156 watermark=52376 size=65536 t=91422"),
        Some((52376, 13156, 52376, 65536))
    );
    assert_eq!(
        parse_heap_line("[INFO!] [PERSISTENT_LOG] [HEAP] used=1 free=2 watermark=3 size=4 t=9 t=2"),
        Some((1, 2, 3, 4))
    );
}

/// The stamp is only a measurement if the clock behind it was running, and
/// only usable if it is on every line. Both are properties of the firmware
/// source, so both are pinned here.
#[test]
fn the_firmware_stamps_every_line_with_board_uptime() {
    let log = nrf_source("log.rs");
    assert!(
        log.contains("embassy_time::Instant::now().as_millis()"),
        "leviculum-nrf/src/log.rs no longer reads board uptime for the t= stamp"
    );
    // Uniform: both formatting paths — the `log_fmt*` one and the tracing
    // subscriber's hand-built line — close through the shared shaper. A
    // stamp on some lines and not others is a trap for whoever reads the
    // log next.
    assert!(
        log.contains("leviculum_log_line::format_line("),
        "log_fmt/log_fmt_critical no longer shape their line through leviculum-log-line"
    );
    assert!(
        log.contains("leviculum_log_line::finish("),
        "the tracing subscriber no longer appends the t= stamp"
    );
    // The stamp is meaningful in every logging context only because RTC1 is
    // already running when the first line is written. Both entry points start
    // it — `embassy_nrf::init` — before they log anything. If that order ever
    // flips, the boot lines silently become t=0.
    for bin in ["bin/t114.rs", "bin/rak4631.rs"] {
        let src = nrf_source(bin);
        let init = src
            .find("embassy_nrf::init(")
            .unwrap_or_else(|| panic!("leviculum-nrf/src/{bin} no longer calls embassy_nrf::init"));
        let first_log = ["log_critical!", "info!(", "warn!(", "log_fmt"]
            .iter()
            .filter_map(|pat| src.find(pat))
            .min()
            .unwrap_or_else(|| panic!("leviculum-nrf/src/{bin} logs nothing at all"));
        assert!(
            init < first_log,
            "leviculum-nrf/src/{bin} logs before embassy_nrf::init starts the clock, \
             so the first lines would carry t=0 rather than a real uptime"
        );
    }
}

/// The panic handler and the HardFault exception must keep NOT logging.
///
/// They are the two contexts where a clock read would have needed a sentinel.
/// Today neither writes a log line: both capture their evidence into
/// `.uninit` RAM and `sys_reset`, and the NEXT boot logs it with a running
/// clock. That is what makes "every line carries a real stamp" true without
/// exception, so it is asserted rather than merely described.
/// The brace-balanced body that follows `marker` in `src`.
fn body_after<'a>(src: &'a str, marker: &str, what: &str) -> &'a str {
    let after = src
        .split_once(marker)
        .unwrap_or_else(|| panic!("leviculum-nrf/src/lib.rs: {what} not found ({marker:?})"))
        .1;
    let open = after
        .find('{')
        .unwrap_or_else(|| panic!("leviculum-nrf/src/lib.rs: {what} has no body"));
    let mut depth = 0usize;
    for (i, c) in after[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &after[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("leviculum-nrf/src/lib.rs: {what} body is not brace-balanced");
}

#[test]
fn the_fault_paths_still_capture_rather_than_log() {
    let lib = nrf_source("lib.rs");
    for (marker, what) in [
        ("#[panic_handler]", "the panic handler"),
        ("unsafe fn HardFault(", "the HardFault handler"),
    ] {
        let body = body_after(&lib, marker, what);
        assert!(
            !body.contains("log_fmt") && !body.contains("log_critical!"),
            "{what} now logs; it runs with the executor dead, and whether its line \
             can carry a real t= stamp is a decision that has to be made explicitly \
             rather than inherited"
        );
    }
}
