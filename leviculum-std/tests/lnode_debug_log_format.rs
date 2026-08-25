//! Codeberg #65 instrumentation contract: the shape of the LNode debug lines
//! the host side greps.
//!
//! The firmware emits two line shapes on the CDC-ACM debug console:
//!
//! ```text
//! [INFO!] [PANIC_COUNT] total=<u32> t=<ms>                  (boot banner, once per boot)
//! [HEAP] used=<n> free=<n> watermark=<n> size=<n> t=<ms>    (every 30 s)
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
