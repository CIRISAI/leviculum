//! Codeberg #65 instrumentation contract: the shape of the LNode debug lines
//! the host side greps.
//!
//! The firmware emits two line shapes on the CDC-ACM debug console:
//!
//! ```text
//! [INFO!] [PANIC_COUNT] total=<u32>                  (boot banner, once per boot)
//! [HEAP] used=<n> free=<n> watermark=<n> size=<n>    (every 30 s)
//! ```
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
    for bin in ["bin/t114.rs", "bin/rak4631.rs"] {
        assert!(
            nrf_source(bin).contains("[PANIC_COUNT] total="),
            "leviculum-nrf/src/{bin} no longer emits the [PANIC_COUNT] banner"
        );
    }
}
