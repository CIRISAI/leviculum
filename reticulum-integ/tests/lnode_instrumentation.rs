//! Codeberg #65 instrumentation contract + live check.
//!
//! The nightly OOM analysis greps LNode debug captures for two line
//! shapes introduced with the #65 instrumentation:
//!
//! ```text
//! [INFO!] [PANIC_COUNT] total=<u32>          (boot banner, once per boot)
//! [HEAP] used=<n> free=<n> watermark=<n> size=<n>   (every 30 s)
//! ```
//!
//! The parse-level tests pin that format against real captured lines —
//! if the firmware changes the shape, these fail before the nightly
//! analysis silently greps nothing. The `lora_lnode_instrumentation`
//! live test (nightly, `--include-ignored`, profile `lnode_pair`)
//! listens on every attached LNode's debug port and asserts the heap
//! telemetry is really flowing.

use std::time::{Duration, Instant};

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

/// Live check on the rig (nightly): every attached LNode emits `[HEAP]`
/// telemetry within ~70 s (cadence is 30 s), with sane values.
/// Read-only listen on the debug ports — no resets, no radio traffic.
#[test]
#[ignore = "hardware-bound: requires LNode debug ports (nightly --include-ignored)"]
fn lora_lnode_instrumentation() {
    reticulum_integ::lock::acquire_integ_lock();
    let devices = reticulum_integ::runner::get_discovered_devices();
    assert!(
        !devices.lnodes.is_empty(),
        "no LNode attached — the lnode_pair profile should have powered them"
    );

    for lnode in &devices.lnodes {
        let mut port = serialport::new(&lnode.debug_port, 115_200)
            .timeout(Duration::from_secs(2))
            .open()
            .unwrap_or_else(|e| panic!("open {}: {e}", lnode.debug_port));
        let _ = port.write_data_terminal_ready(true);

        let deadline = Instant::now() + Duration::from_secs(70);
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut parsed = None;
        'listen: while Instant::now() < deadline {
            match std::io::Read::read(&mut port, &mut chunk) {
                Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                _ => {}
            }
            for line in String::from_utf8_lossy(&buf).lines() {
                if let Some(values) = parse_heap_line(line) {
                    parsed = Some(values);
                    break 'listen;
                }
            }
        }

        let (used, free, watermark, size) = parsed.unwrap_or_else(|| {
            panic!(
                "LNode {} ({}): no [HEAP] telemetry within 70s — instrumented firmware missing?",
                lnode.usb_serial, lnode.debug_port
            )
        });
        assert!(used > 0, "{}: used must be > 0", lnode.usb_serial);
        assert!(free > 0, "{}: free must be > 0", lnode.usb_serial);
        assert!(
            size > 0 && used + free <= size,
            "{}: used+free <= size",
            lnode.usb_serial
        );
        assert!(
            watermark >= used && watermark <= size,
            "{}: watermark within [used, size]",
            lnode.usb_serial
        );
        eprintln!(
            "LNode {}: [HEAP] used={used} free={free} watermark={watermark} size={size} OK",
            lnode.usb_serial
        );
    }
}
