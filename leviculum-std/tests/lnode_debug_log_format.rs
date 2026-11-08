//! Codeberg #65 instrumentation contract: the shape of the LNode debug lines
//! the host side greps.
//!
//! The firmware emits these line shapes on the CDC-ACM debug console:
//!
//! ```text
//! [INFO!] [PANIC_COUNT] total=<u32> t=<ms>                  (boot banner, once per boot)
//! [HEAP] used=<n> free=<n> watermark=<n> size=<n> t=<ms>    (every 30 s)
//! [SX_REG] rxgain_before=0xNN rxgain_after=0xNN txmod=0xNN  (once, end of init_radio)
//! [SX_REG_IQ] iq_before=0xNN iq_after=0xNN txmod=0xNN       (once, first SetPacketParams)
//! [SX_RX_ARM] site=<tag> timeout_ms=<u32> dark_ms=<u64|first>  (every SetRx)
//! [SX_RX_ADOPT] latched=0xNNNN preamble=<0|1> header=<0|1> rxdone=<0|1> stood_ms=<u32>
//! [SX_RX_TEARDOWN] site=<tag> preamble=<0|1> header=<0|1> rxdone=<0|1> armed_ms=<u32> waited_ms=<u64>
//! [SX_RX_HARVEST] site=<tag> preamble=<0|1> header=<0|1> rxdone=<0|1> armed_ms=<u32> waited_ms=<u64>
//! [SX_TX_DEFER] waited_ms=<u64> reason=<preamble|header> outcome=<frame|timeout|abandoned|false_preamble>
//! [BLE ] BLE_DIAL_REFUSED addr=<12 hex> lived_ms=<u64> fallback_in_ms=<u64>  (a dial of ours the peer refused)
//! ```
//!
//! The adopt/teardown pair is read as a rate against each other: an
//! `[SX_RX_ADOPT]` with any flag set is a reception the pre-adoption firmware
//! destroyed, an `[SX_RX_TEARDOWN]` with any flag set is one still being
//! destroyed, and `site=` on the teardown says by which caller. Both are
//! emitted with the flags as read, all-zero included; a line that appeared
//! only when it had bad news would give a numerator with no denominator.
//!
//! `[SX_RX_HARVEST]` is the teardown's other half, at the same `site=` and in
//! the same fields: a window whose reception had already completed when a
//! transmit asked for it, taken rather than destroyed. Read as a fraction of
//! the teardowns carrying `rxdone=1` at that site it is the rate at which that
//! collision still costs a frame — which, after the fix that introduced the
//! line, should be the readout failures and nothing else. The shared field
//! order is why `parse_rx_latch` below takes the tag as an argument.
//!
//! `[SX_TX_DEFER]` is the third of that family and the one that reports a
//! behaviour rather than an observation: a transmit that found a reception
//! arriving on the window it was about to end waited for it instead.
//! `outcome=frame` against `outcome=timeout`, taken per `reason=`, is whether
//! the wait is earning its keep or merely delaying the transmitter — the ratio
//! the guard has to justify itself with, and the reason the line carries the
//! measured `waited_ms` rather than the bound it was allowed.
//! `outcome=false_preamble` is the carrier-detect site's own release: a carrier
//! that showed no header inside the preamble-plus-header time, let go at a
//! tenth of the frame bound rather than at it (Codeberg #426).
//!
//! `waited_ms=` on the teardown and the harvest is the same quantity for the
//! same reason, and it is `0` on every site that does not wait. It is read as
//! an OPTIONAL trailing field below, unlike every other field on those lines:
//! the parser has to keep reading the archived captures and the persistent-log
//! replays of boards on firmware older than #426, which carry the line without
//! it. What guards the emission instead is the format-string pin in
//! `the_firmware_still_emits_both_halves_of_the_adoption_instrument`.
//!
//! The trailing ` t=<ms>` is board uptime at the moment the line was
//! formatted, appended to EVERY runtime line since the drain-latency audit
//! (#344). It is deliberately at the END: every consumer below anchors on a
//! `[TAG]`, and a leading stamp would break all of them at once. The parsers
//! here therefore have to keep working with an unknown-key field appended —
//! which is exactly what `stamp_does_not_disturb_the_existing_parsers` asserts.
//! The stamp is the renderer's alone: a body that appends its own prints `t=`
//! twice, which `no_firmware_log_body_carries_its_own_stamp` refuses (890).
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
//!
//! `just fast` does not execute this file: it ends at `cargo test --workspace
//! --lib`, and its `check-all-targets` dependency (Codeberg #220) only
//! *compiles* `tests/` targets, so an assertion here first runs in the landing
//! gate's `cargo test --workspace`. A batch that changes what these tests pin
//! — the firmware's log grammar, or the source invariants below — has to name
//! `cargo test -p leviculum-std --test lnode_debug_log_format` itself.

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
    tree_source("leviculum-nrf/src", rel)
}

/// Does this board binary run the shared node loop (#469 step 7, order
/// 1085)? The boards move onto it one order at a time; until one has, its
/// binary holds every line of its boot itself.
fn on_node_loop(bin_src: &str) -> bool {
    bin_src.contains("node_loop::run(")
}

/// A board binary's boot path as text: the binary and, for a binary on the
/// shared node loop, the loop's source behind it with `crate::` spelt
/// `leviculum_nrf::`, the path the same code had while it lived in the
/// binary. The invariants below are about what a board runs, not about
/// which file of the crate a line sits in.
fn board_boot_path(bin: &str) -> String {
    let src = nrf_source(bin);
    if on_node_loop(&src) {
        let skeleton = nrf_source("node_loop.rs").replace("crate::", "leviculum_nrf::");
        format!("{src}\n{skeleton}")
    } else {
        src
    }
}

/// A source of the orchestration stratum, `leviculum-nrf/firmware/src`, where
/// the chip-free modules move out of the bind crate (#469).
fn firmware_source(rel: &str) -> String {
    tree_source("leviculum-nrf/firmware/src", rel)
}

fn tree_source(tree: &str, rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(tree)
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
    let heap = firmware_source("heap.rs");
    assert!(
        heap.contains(r#""[HEAP] ""#),
        "leviculum-nrf/firmware/src/heap.rs no longer emits the `[HEAP] ` tag"
    );
    assert!(
        heap.contains("used={used} free={free} watermark={watermark} size={HEAP_SIZE}"),
        "the [HEAP] field order or spelling changed in leviculum-nrf/firmware/src/heap.rs"
    );
    // The banner format string lives in the shared helper (48f34f6 moved it
    // out of the BSP bins), and so does the whole boot-diagnostics block and
    // the periodic watermarks: each bin must still invoke both helpers. The
    // RAK4631 bin once logged no reset reason and spawned no `[STACK]`
    // watermark (review finding L-0031), so all three bins are checked.
    assert!(
        firmware_source("postmortem.rs").contains("[PANIC_COUNT] total="),
        "leviculum-nrf/firmware/src/postmortem.rs no longer emits the [PANIC_COUNT] banner"
    );
    let boot_diag = fn_body(&lib, "pub fn log_boot_diagnostics(");
    for call in [
        "log_panic_count()",
        "log_reset_reason(",
        "log_stack(\"boot\")",
    ] {
        assert!(
            boot_diag.contains(call),
            "log_boot_diagnostics no longer calls {call}"
        );
    }
    let spawn = fn_body(&lib, "pub fn spawn_diagnostics_tasks(");
    assert!(
        spawn.contains("stack_watermark_task()") && spawn.contains("heap_watermark_task()"),
        "spawn_diagnostics_tasks no longer spawns both watermark tasks"
    );
    for bin in ["bin/t114.rs", "bin/rak4631.rs", "bin/solarnode.rs"] {
        let src = board_boot_path(bin);
        assert!(
            src.contains("leviculum_nrf::log_boot_diagnostics("),
            "leviculum-nrf/src/{bin} no longer logs the shared boot diagnostics"
        );
        assert!(
            src.contains("leviculum_nrf::spawn_diagnostics_tasks("),
            "leviculum-nrf/src/{bin} no longer spawns the shared diagnostics tasks"
        );
    }
}

/// The text of the function whose signature starts with `sig`, up to the
/// first line that closes it at column 0.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("leviculum-nrf/src/lib.rs has no `{sig}`"));
    let rest = &src[start..];
    let end = rest.find("\n}\n").map_or(rest.len(), |i| i + 3);
    &rest[..end]
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
    let log = firmware_source("log.rs");
    assert!(
        log.contains("embassy_time::Instant::now().as_millis()"),
        "leviculum-nrf/firmware/src/log.rs no longer reads board uptime for the t= stamp"
    );
    // Uniform: the one formatting path, `log_fmt*`, closes through the
    // shared shaper. A stamp on some lines and not others is a trap for
    // whoever reads the log next. (A second path, the tracing subscriber's
    // hand-built line, was deleted with the subscriber: the firmware builds
    // leviculum-core without `tracing`, so it never ran.)
    assert!(
        log.contains("leviculum_log_line::format_line("),
        "log_fmt/log_fmt_critical no longer shape their line through leviculum-log-line"
    );
    // The stamp is meaningful in every logging context only because RTC1 is
    // already running when the first line is written. Both entry points start
    // it — `embassy_nrf::init` — before they log anything. If that order ever
    // flips, the boot lines silently become t=0.
    // A binary on the shared node loop starts the clock through
    // `node_loop::enter`, its first call (#469 step 7, order 1085): there
    // the call has to precede the binary's first line, and in the loop's
    // source `embassy_nrf::init` has to precede every line.
    let first_log = |src: &str, file: &str| {
        ["log_critical!", "info!(", "warn!(", "log_fmt"]
            .iter()
            .filter_map(|pat| src.find(pat))
            .min()
            .unwrap_or_else(|| panic!("leviculum-nrf/src/{file} logs nothing at all"))
    };
    let starts_clock_before_logging = |file: &str, src: &str, start: &str| {
        let init = src
            .find(start)
            .unwrap_or_else(|| panic!("leviculum-nrf/src/{file} no longer calls {start}"));
        assert!(
            init < first_log(src, file),
            "leviculum-nrf/src/{file} logs before {start} starts the clock, \
             so the first lines would carry t=0 rather than a real uptime"
        );
    };
    let mut on_loop = 0;
    for bin in ["bin/t114.rs", "bin/rak4631.rs"] {
        let src = nrf_source(bin);
        if on_node_loop(&src) {
            on_loop += 1;
            starts_clock_before_logging(bin, &src, "node_loop::enter(");
        } else {
            starts_clock_before_logging(bin, &src, "embassy_nrf::init(");
        }
    }
    if on_loop > 0 {
        starts_clock_before_logging(
            "node_loop.rs",
            &nrf_source("node_loop.rs"),
            "embassy_nrf::init(",
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
fn body_after<'a>(src: &'a str, file: &str, marker: &str, what: &str) -> &'a str {
    let after = src
        .split_once(marker)
        .unwrap_or_else(|| panic!("leviculum-nrf/src/{file}: {what} not found ({marker:?})"))
        .1;
    let open = after
        .find('{')
        .unwrap_or_else(|| panic!("leviculum-nrf/src/{file}: {what} has no body"));
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
    panic!("leviculum-nrf/src/{file}: {what} body is not brace-balanced");
}

#[test]
fn the_fault_paths_still_capture_rather_than_log() {
    let lib = nrf_source("lib.rs");
    // The handlers stay in the bind crate; the captures they call moved to
    // the orchestration crate's post-mortem store (#469), and run in the same
    // dead-executor context, so they are held to the same rule.
    let store = firmware_source("postmortem.rs");
    for (src, file, marker, what) in [
        (&lib, "lib.rs", "#[panic_handler]", "the panic handler"),
        (
            &lib,
            "lib.rs",
            "unsafe fn HardFault(",
            "the HardFault handler",
        ),
        (
            &store,
            "../firmware/src/postmortem.rs",
            "pub fn record_panic(",
            "the panic capture",
        ),
        (
            &store,
            "../firmware/src/postmortem.rs",
            "pub fn record_hardfault(",
            "the HardFault capture",
        ),
        (
            &store,
            "../firmware/src/postmortem.rs",
            "pub fn bump_panic_count(",
            "the panic counter",
        ),
    ] {
        let body = body_after(src, file, marker, what);
        assert!(
            !body.contains("log_fmt") && !body.contains("log_critical!"),
            "{what} now logs; it runs with the executor dead, and whether its line \
             can carry a real t= stamp is a decision that has to be made explicitly \
             rather than inherited"
        );
    }
    for capture in [
        "record_panic(info)",
        "record_hardfault(",
        "bump_panic_count()",
    ] {
        assert!(
            lib.contains(capture),
            "the fault handlers in leviculum-nrf/src/lib.rs no longer call {capture}"
        );
    }
}

/// Extract `(site, timeout_ms, dark_ms)` from an `[SX_RX_ARM]` line.
///
/// `dark_ms` is `Some(None)` for the boot arm, which carries the word `first`
/// rather than a digit: there is no previous window to measure from, and the
/// outer `Some` says the field was present and understood. A consumer that
/// coerced that to zero would put a fabricated measurement — "the radio was
/// re-armed instantly" — into the population this line exists to characterise.
#[allow(clippy::type_complexity)]
fn parse_rx_arm(line: &str) -> Option<(String, u32, Option<u64>)> {
    let idx = line.find("[SX_RX_ARM] ")?;
    let rest = &line[idx + "[SX_RX_ARM] ".len()..];
    let mut site = None;
    let mut timeout_ms = None;
    let mut dark_ms = None;
    for token in rest.split_whitespace() {
        let (key, value) = token.split_once('=')?;
        match key {
            "site" => site = Some(value.to_string()),
            "timeout_ms" => timeout_ms = Some(value.parse().ok()?),
            "dark_ms" => {
                dark_ms = Some(if value == "first" {
                    None
                } else {
                    Some(value.parse().ok()?)
                })
            }
            _ => {} // the ` t=` stamp, and anything appended after it
        }
    }
    Some((site?, timeout_ms?, dark_ms?))
}

/// The receiver-arming line parses, in every shape the firmware emits it.
///
/// This is the line that makes "was the radio listening at instant X"
/// readable off a capture instead of inferred: `t=` is the instant the
/// receiver went live and `dark_ms` closes the span back to the previous
/// window's end. Both halves are useless if a consumer cannot split them out.
#[test]
fn receiver_arming_line_parses() {
    assert_eq!(
        parse_rx_arm("[SX_RX_ARM] site=idle timeout_ms=0 dark_ms=3 t=123456"),
        Some(("idle".to_string(), 0, Some(3)))
    );
    assert_eq!(
        parse_rx_arm("[SX_RX_ARM] site=ack timeout_ms=2590 dark_ms=0 t=91422"),
        Some(("ack".to_string(), 2590, Some(0)))
    );
    // The boot arm. Not a zero, and a parser must not turn it into one.
    assert_eq!(
        parse_rx_arm("[SX_RX_ARM] site=idle timeout_ms=0 dark_ms=first t=412"),
        Some(("idle".to_string(), 0, None))
    );
    // A replay of the previous boot's tail still parses.
    assert_eq!(
        parse_rx_arm(
            "[INFO!] [PERSISTENT_LOG] [SX_RX_ARM] site=yield timeout_ms=5180 dark_ms=461 t=9 t=2"
        ),
        Some(("yield".to_string(), 5180, Some(461)))
    );
    // A missing field is a parse failure, not a default.
    assert_eq!(
        parse_rx_arm("[SX_RX_ARM] site=csma timeout_ms=400 t=7"),
        None
    );
    assert_eq!(
        parse_rx_arm("[T114_LORA_LOOP] op=rx_timeout duration_ms=500"),
        None
    );
}

/// The two windows a capture must be able to tell apart carry different tags.
///
/// The six sites are the loop's six listening windows. Their timeouts
/// overlap — the airtime hold and the ack window are the same length by
/// construction — so the tag is the only thing distinguishing them.
#[test]
fn every_listening_window_has_its_own_tag() {
    let hold = "[SX_RX_ARM] site=hold timeout_ms=2590 dark_ms=1 t=1000";
    let ack = "[SX_RX_ARM] site=ack timeout_ms=2590 dark_ms=1 t=2000";
    let (hold_site, hold_to, _) = parse_rx_arm(hold).expect("hold parses");
    let (ack_site, ack_to, _) = parse_rx_arm(ack).expect("ack parses");
    assert_eq!(
        hold_to, ack_to,
        "the two windows really are the same length"
    );
    assert_ne!(hold_site, ack_site);
}

/// The arming line does not disturb the parsers that came before it.
#[test]
fn the_arming_line_is_not_mistaken_for_another_line() {
    assert_eq!(
        parse_heap_line("[SX_RX_ARM] site=idle timeout_ms=0 dark_ms=3 t=1"),
        None
    );
    assert_eq!(
        parse_stamp("[SX_RX_ARM] site=idle timeout_ms=0 dark_ms=3 t=123456"),
        Some(123456)
    );
    // `dark_ms=first` is a word in a numeric-looking field; the stamp parser
    // must still find the stamp past it.
    assert_eq!(
        parse_stamp("[SX_RX_ARM] site=idle timeout_ms=0 dark_ms=first t=412"),
        Some(412)
    );
}

/// The firmware still emits the arming line, at the `SetRx` and nowhere else,
/// and from every one of the six windows.
///
/// Three separate facts, and the middle one is the load-bearing one: a line
/// logged anywhere but immediately after the `SET_RX` command would carry a
/// `t=` that is not the instant the receiver went live, and the whole span
/// arithmetic downstream would be off by however much code sits in between.
#[test]
fn the_firmware_still_emits_the_receiver_arming_line() {
    let sx = nrf_source("sx1262.rs");
    assert!(
        sx.contains(r#""[SX_RX_ARM] ""#),
        "leviculum-nrf/src/sx1262.rs no longer emits the `[SX_RX_ARM] ` tag"
    );
    // The arm report is taken after the SetRx write and before the DIO1 wait.
    let set_rx = sx
        .find("self.write_command(opcode::SET_RX, &t).await?;")
        .expect("leviculum-nrf/src/sx1262.rs no longer issues SetRx");
    // Matched on `.arm(` rather than the whole receiver expression: rustfmt
    // splits `self.rx_arm.arm(..)` across lines once the argument list grows,
    // and a pin that a reformat can break is a pin nobody keeps.
    // `bound_ms`, not `timeout_ms`: the line carries the bound the chip's
    // timer actually runs — the ask plus the transmit-gating sites' settle
    // pad (929, #466) — so a capture's `timeout_ms=` stays matchable against
    // the expiry that follows it.
    let arm = sx
        .find(".arm(site, bound_ms,")
        .expect("leviculum-nrf/src/sx1262.rs no longer reports the arming");
    let wait = sx[set_rx..]
        .find("self.dio1.wait_for_high()")
        .map(|i| i + set_rx)
        .expect("leviculum-nrf/src/sx1262.rs no longer waits on DIO1 after SetRx");
    assert!(
        set_rx < arm && arm < wait,
        "the [SX_RX_ARM] stamp is not taken between the SetRx and the DIO1 \
         wait, so its t= is not the instant the receiver went live"
    );
    // The window end is recorded on the far side of that wait, which is what
    // makes dark_ms a gap rather than a window length.
    let ended = sx[wait..]
        .find(".window_ended(")
        .map(|i| i + wait)
        .expect("leviculum-nrf/src/sx1262.rs no longer records the window end");
    assert!(
        ended > wait,
        "the window end is recorded before the DIO1 wait, so dark_ms would \
         span a window rather than the gap between two"
    );
    // All six windows are tagged, and each exactly once: two windows sharing
    // a tag is a capture nobody can read back apart.
    let lora = nrf_source("lora.rs");
    for site in ["Idle", "Ack", "Csma", "Jitter", "Hold", "Yield"] {
        assert_eq!(
            lora.matches(&format!("RxSite::{site}")).count(),
            1,
            "leviculum-nrf/src/lora.rs does not arm exactly one window as RxSite::{site}"
        );
    }
    // The field names and their order are the parser's contract, and they
    // live in core's Display impl.
    let core_src = {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("leviculum-core/src/sx126x.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };
    assert!(
        core_src.contains(r#""site={} timeout_ms={} dark_ms=""#),
        "the [SX_RX_ARM] field order or spelling changed in leviculum-core/src/sx126x.rs"
    );
    assert!(
        core_src.contains(r#"pub const DARK_MS_FIRST: &str = "first";"#),
        "the [SX_RX_ARM] no-previous-window sentinel changed in \
         leviculum-core/src/sx126x.rs; parse_rx_arm above still expects `first`"
    );
}

/// Extract `(preamble, header, rxdone, age_ms)` from an `[SX_RX_ADOPT]` or an
/// `[SX_RX_TEARDOWN]` line.
///
/// `age_ms` is `stood_ms` on the one and `armed_ms` on the other — the same
/// quantity, a window's age measured from its own arming, named for what the
/// line is about. Both are read as rates over the whole population, so a
/// consumer that skipped the all-zero lines would compute a numerator against
/// no denominator; there is deliberately nothing here that filters them.
fn parse_rx_latch(tag: &str, line: &str) -> Option<(bool, bool, bool, u32)> {
    let idx = line.find(&format!("[{tag}] "))?;
    let rest = &line[idx + tag.len() + 3..];
    let (mut preamble, mut header, mut rxdone, mut age) = (None, None, None, None);
    let bit = |v: &str| match v {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    for token in rest.split_whitespace() {
        let (key, value) = token.split_once('=')?;
        match key {
            "preamble" => preamble = Some(bit(value)?),
            "header" => header = Some(bit(value)?),
            "rxdone" => rxdone = Some(bit(value)?),
            "stood_ms" | "armed_ms" => age = Some(value.parse().ok()?),
            _ => {} // `latched=`, `site=`, the ` t=` stamp, anything appended
        }
    }
    Some((preamble?, header?, rxdone?, age?))
}

/// The adoption pair parses, in both shapes and including the all-zero case.
#[test]
fn the_adopt_and_teardown_lines_parse() {
    assert_eq!(
        parse_rx_latch(
            "SX_RX_ADOPT",
            "[SX_RX_ADOPT] latched=0x0016 preamble=1 header=1 rxdone=1 stood_ms=20 t=1421792"
        ),
        Some((true, true, true, 20))
    );
    // The denominator. A window adopted with an empty channel behind it is
    // still a sample, and it is the majority of them.
    assert_eq!(
        parse_rx_latch(
            "SX_RX_ADOPT",
            "[SX_RX_ADOPT] latched=0x0000 preamble=0 header=0 rxdone=0 stood_ms=101 t=9"
        ),
        Some((false, false, false, 101))
    );
    assert_eq!(
        parse_rx_latch(
            "SX_RX_TEARDOWN",
            "[SX_RX_TEARDOWN] site=arm preamble=1 header=0 rxdone=0 armed_ms=214 t=42"
        ),
        Some((true, false, false, 214))
    );
    // The harvest renders the teardown's fields, so one parser reads both and
    // the saved and the lost half of a site are directly comparable.
    assert_eq!(
        parse_rx_latch(
            "SX_RX_HARVEST",
            "[SX_RX_HARVEST] site=select preamble=1 header=1 rxdone=1 armed_ms=10098 t=266026"
        ),
        Some((true, true, true, 10098))
    );
    // A replay of the previous boot's tail still parses.
    assert_eq!(
        parse_rx_latch(
            "SX_RX_TEARDOWN",
            "[INFO!] [PERSISTENT_LOG] [SX_RX_TEARDOWN] site=select preamble=0 header=0 \
             rxdone=0 armed_ms=3 t=9 t=2"
        ),
        Some((false, false, false, 3))
    );
    // The teardown a wait preceded (Codeberg #426), and the same line from a
    // board on firmware that predates the field: both are samples of the same
    // population and the parser reads both, which is why `waited_ms` is the one
    // optional field on this line.
    assert_eq!(
        parse_rx_latch(
            "SX_RX_TEARDOWN",
            "[SX_RX_TEARDOWN] site=cad preamble=1 header=0 rxdone=0 armed_ms=78 \
             waited_ms=78 t=114658"
        ),
        Some((true, false, false, 78))
    );
    assert_eq!(
        parse_rx_latch(
            "SX_RX_TEARDOWN",
            "[SX_RX_TEARDOWN] site=cad preamble=1 header=0 rxdone=0 armed_ms=41 t=114580"
        ),
        Some((true, false, false, 41))
    );
    // A missing field is a parse failure, not a default, and a flag that is
    // not a bit is not a `false`.
    assert_eq!(
        parse_rx_latch(
            "SX_RX_TEARDOWN",
            "[SX_RX_TEARDOWN] site=tx preamble=0 header=0 armed_ms=3"
        ),
        None
    );
    assert_eq!(
        parse_rx_latch(
            "SX_RX_ADOPT",
            "[SX_RX_ADOPT] latched=0x0000 preamble=no header=0 rxdone=0 stood_ms=3"
        ),
        None
    );
    // The two do not answer for each other, and neither answers for the arm.
    assert_eq!(
        parse_rx_latch(
            "SX_RX_ADOPT",
            "[SX_RX_ARM] site=idle timeout_ms=0 dark_ms=1"
        ),
        None
    );
    assert_eq!(
        parse_rx_arm("[SX_RX_TEARDOWN] site=tx preamble=0 header=0 rxdone=0 armed_ms=3"),
        None
    );
}

/// The firmware still emits both halves of the adoption instrument, and the
/// field order they are read in is still the one the crate renders.
///
/// The pair only means anything together: without the teardown line the
/// adoptions are a count with nothing to compare against, and without the
/// adoption line the fix cannot report what it saved — after it lands, the
/// loss it removed can no longer be measured any other way.
#[test]
fn the_firmware_still_emits_both_halves_of_the_adoption_instrument() {
    let sx = nrf_source("sx1262.rs");
    for tag in ["[SX_RX_ADOPT] ", "[SX_RX_TEARDOWN] ", "[SX_RX_HARVEST] "] {
        // The quoted form: the tag as a string literal in the source, so a
        // mention in a comment does not satisfy the pin.
        assert!(
            sx.contains(&format!("\"{tag}\"")),
            "leviculum-nrf/src/sx1262.rs no longer emits the `{tag}` tag"
        );
    }
    let crate_src = {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("leviculum-nrf/rx-arming/src/lib.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };
    for shape in [
        r#""preamble={} header={} rxdone={}""#,
        r#""latched={:#06x} {} stood_ms={}""#,
        r#""site={} {} armed_ms={} waited_ms={}""#,
    ] {
        assert!(
            crate_src.contains(shape),
            "the field order or spelling of {shape} changed in \
             leviculum-nrf/rx-arming/src/lib.rs; parse_rx_latch above still \
             expects it"
        );
    }
}

/// Extract `(waited_ms, reason, outcome)` from an `[SX_TX_DEFER]` line.
///
/// All three fields are required. `waited_ms` alone says how much airtime the
/// transmitter gave up and nothing about what it bought; `outcome` alone is a
/// count with no cost attached; and without `reason` the two evidence bits —
/// a bare preamble and a decoded header, which earn the same bound and are not
/// the same evidence — collapse into one population that cannot be separated
/// afterwards.
fn parse_tx_defer(line: &str) -> Option<(u64, String, String)> {
    let idx = line.find("[SX_TX_DEFER] ")?;
    let rest = &line[idx + "[SX_TX_DEFER] ".len()..];
    let (mut waited, mut reason, mut outcome) = (None, None, None);
    for token in rest.split_whitespace() {
        let (key, value) = token.split_once('=')?;
        match key {
            "waited_ms" => waited = Some(value.parse().ok()?),
            "reason" => {
                reason = match value {
                    "preamble" | "header" => Some(value.to_string()),
                    _ => return None,
                }
            }
            "outcome" => {
                outcome = match value {
                    "frame" | "timeout" | "abandoned" | "false_preamble" => Some(value.to_string()),
                    _ => return None,
                }
            }
            _ => {} // the ` t=` stamp, and anything appended after it
        }
    }
    Some((waited?, reason?, outcome?))
}

/// The deferral line parses, in every outcome it can report.
#[test]
fn the_tx_defer_line_parses() {
    assert_eq!(
        parse_tx_defer("[SX_TX_DEFER] waited_ms=446 reason=header outcome=frame t=1053832"),
        Some((446, "header".into(), "frame".into()))
    );
    // The bound expiring is the case the ratio is computed against, so it has
    // to parse just as readily as the repaid one.
    assert_eq!(
        parse_tx_defer("[SX_TX_DEFER] waited_ms=728 reason=preamble outcome=timeout t=9"),
        Some((728, "preamble".into(), "timeout".into()))
    );
    assert_eq!(
        parse_tx_defer(
            "[INFO!] [PERSISTENT_LOG] [SX_TX_DEFER] waited_ms=60 reason=header \
             outcome=abandoned t=9 t=2"
        ),
        Some((60, "header".into(), "abandoned".into()))
    );
    // The carrier-detect site's release (Codeberg #426): a fraction of the
    // frame bound, and its own outcome so the cheap waits are separable from
    // the ones that spent the whole thing.
    assert_eq!(
        parse_tx_defer(
            "[SX_TX_DEFER] waited_ms=78 reason=preamble outcome=false_preamble t=114580"
        ),
        Some((78, "preamble".into(), "false_preamble".into()))
    );
    // A missing field is a parse failure, not a default: a line with no
    // outcome would otherwise be counted as one.
    assert_eq!(
        parse_tx_defer("[SX_TX_DEFER] waited_ms=1 reason=header"),
        None
    );
    // A vocabulary this parser does not know is a firmware that moved, not a
    // value to pass through.
    assert_eq!(
        parse_tx_defer("[SX_TX_DEFER] waited_ms=1 reason=cad outcome=frame"),
        None
    );
    assert_eq!(
        parse_tx_defer("[SX_TX_DEFER] waited_ms=1 reason=header outcome=deferred"),
        None
    );
    // And it does not answer for its neighbours in the same family.
    assert_eq!(
        parse_tx_defer("[SX_RX_TEARDOWN] site=select preamble=1 header=1 rxdone=0 armed_ms=446"),
        None
    );
    assert_eq!(
        parse_rx_latch(
            "SX_RX_TEARDOWN",
            "[SX_TX_DEFER] waited_ms=1 reason=header outcome=frame"
        ),
        None
    );
}

/// The firmware still emits the deferral line, in the grammar the parser above
/// reads.
///
/// The starvation argument the line exists to support is a separate test, one
/// screen down.
#[test]
fn the_firmware_still_emits_the_transmit_deferral_line() {
    let sx = nrf_source("sx1262.rs");
    assert!(
        sx.contains(r#""[SX_TX_DEFER] ""#),
        "leviculum-nrf/src/sx1262.rs no longer emits the `[SX_TX_DEFER] ` tag"
    );
    let crate_src = {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("leviculum-nrf/rx-arming/src/lib.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };
    assert!(
        crate_src.contains(r#""waited_ms={} reason={} outcome={}""#),
        "the [SX_TX_DEFER] field order or spelling changed in \
         leviculum-nrf/rx-arming/src/lib.rs; parse_tx_defer above still expects it"
    );
    for tag in [
        "\"preamble\"",
        "\"header\"",
        "\"frame\"",
        "\"timeout\"",
        "\"abandoned\"",
        "\"false_preamble\"",
    ] {
        assert!(
            crate_src.contains(tag),
            "the [SX_TX_DEFER] vocabulary lost {tag} in \
             leviculum-nrf/rx-arming/src/lib.rs; parse_tx_defer above rejects \
             anything else"
        );
    }
}

/// **Codeberg #410, the positive control for the lease.** The transmit
/// admission has to ask the deadline, not the host's request for one.
///
/// `RadioConfig::radio_silent` is what the host asked for and it is never
/// cleared on the board — nothing rewrites a field of the applied config
/// when a lease runs out except the loop's own expiry arm. So a
/// `admit_for_transmit` that consults the flag is a board that is mute
/// forever again, and the whole of this change is undone by putting one
/// identifier back. That revert compiles, links, flashes and passes every
/// other test in the tree, because `leviculum-nrf` cross-compiles and has no
/// host tests: this is the only thing between it and the rig.
///
/// Asserted on the function's own body rather than on the file, so a
/// `radio_silent` mentioned in a comment somewhere else in `lora.rs` — there
/// are several — cannot make it pass or fail for the wrong reason.
#[test]
fn the_transmit_admission_asks_the_lease_and_not_the_hosts_request() {
    let lora = nrf_source("lora.rs");
    let start = sole_index(&lora, "fn admit_for_transmit(");
    let body = &lora[start..];
    let end = body
        .find("\n}\n")
        .expect("unterminated admit_for_transmit in leviculum-nrf/src/lora.rs");
    let body = &body[..end];

    assert!(
        body.contains("lease.is_muted("),
        "admit_for_transmit no longer asks the mute lease; a host mute is          unbounded again and the board can only be freed by a reset or a          second config (leviculum#410). Body:\n{body}"
    );
    assert!(
        !body.contains("radio_silent"),
        "admit_for_transmit reads `radio_silent` again. That field is the          host's REQUEST for a silence and outlives the lease that bounds it,          so reading it here restores the seven-hour mute of 2026-09-15.          Body:\n{body}"
    );
    assert!(
        !body.contains("config: &RadioConfig"),
        "admit_for_transmit takes the config again; it is handed the lease          precisely so the flag is not in reach. Body:\n{body}"
    );
}

/// The other half of the same defect: the expiry has to be ANNOUNCED. A
/// board that quietly starts transmitting again is as hard to read as one
/// that quietly stopped — the 2026-09-15 diagnosis was made from which lines
/// were absent, and this is the line whose presence answers it.
#[test]
fn the_firmware_still_announces_a_lease_that_ran_out() {
    let lora = nrf_source("lora.rs");
    assert!(
        lora.contains("mute_lease.expired_at("),
        "leviculum-nrf/src/lora.rs no longer asks whether the mute's lease          ran out, so nothing emits LORA_MUTE_EXPIRED"
    );
    assert!(
        lora.contains("facts::lora_mute_expired("),
        "the expiry is no longer written through leviculum_log_line::facts,          so the line's format has left the one place it is asserted"
    );

    let facts: String = {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("leviculum-nrf/log-line/src/facts.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };
    assert!(
        facts.contains("\"LORA_MUTE_EXPIRED lease_s={} dropped={}\""),
        "the LORA_MUTE_EXPIRED format string changed; the rig check for          leviculum#410 greps it verbatim"
    );
}

/// The index of `needle` in `src`, insisting there is exactly one.
///
/// The region slicing below is only meaningful if the markers it cuts on are
/// unique; a second `let idle = select(` in the file would silently give this
/// test the wrong body to look at.
fn sole_index(src: &str, needle: &str) -> usize {
    let mut found = src.match_indices(needle).map(|(i, _)| i);
    let first = found.next().unwrap_or_else(|| {
        panic!(
            "leviculum-nrf/src/lora.rs no longer contains `{needle}`; the region \
             whose arms carry the transmit deferral has moved, and this test cannot \
             see the bound any more"
        )
    });
    assert!(
        found.next().is_none(),
        "`{needle}` appears more than once in leviculum-nrf/src/lora.rs; this test \
         slices the deferring regions on it and can no longer tell them apart"
    );
    first
}

/// The `RxTeardownBy::` site each `disarm_rx_for_tx` call in `src` names, in
/// source order.
///
/// A call whose site argument cannot be read is reported as `<unreadable>`
/// rather than skipped: a site this test cannot name is still a site, and has
/// to fail loudly instead of disappearing from the count.
fn transmit_deferral_sites(src: &str) -> Vec<String> {
    const CALL: &str = "disarm_rx_for_tx(";
    const SITE: &str = "RxTeardownBy::";
    let starts: Vec<usize> = src.match_indices(CALL).map(|(i, _)| i).collect();
    let mut sites = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let call = &src[start..starts.get(n + 1).copied().unwrap_or(src.len())];
        let name = call
            .find(SITE)
            .map(|at| &call[at + SITE.len()..])
            .and_then(|tail| {
                tail.split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or("<unreadable>");
        sites.push(name.to_string());
    }
    sites
}

/// What a future reader has to establish by hand before a third deferring site
/// may land, because no scan of the source can establish it.
const DEFERRAL_HAND_CHECK: &str = "A source scan sees a site's name and that it \
    is spent once per arm; it cannot see the cadence that reaches it. Before a \
    new deferring site lands, check by hand what actually bounds the wait: (a) \
    the arm is entered only on an event produced outside this loop — a daemon \
    dequeue, a host config push — and at that event's rate, never at the loop's \
    own; (b) the call is reached at most once per entry, not from a loop and \
    not twice on one path. The two sites below pass that check, so their waits \
    add at most one maximum-size frame's airtime each and do not compound. The \
    config site needs one step more, because since 2026-09-23 it sits in \
    `rx_window`, which the loop calls at every window it arms: what keeps it at \
    once per push is that a window entered while a config is already waiting is \
    not armed at all, so the first stand-down of a turn is the only one and the \
    turn's top then consumes the config. If a new site passes the check too, \
    name it in the regions below and say here why its cadence is external; if it \
    does not, it must not defer with this bound. A site that cannot pass it may \
    still defer with the other policy: the carrier-detect does, once per CSMA \
    retry, and it pays for that by releasing a bare carrier at a tenth of the \
    bound (Codeberg #426). It is not in this file's regions because it defers \
    inside the driver's own `cad`, not in the loop; the site table that covers \
    every teardown is the mvr \
    `cad_tears_down_the_frame_it_then_detects`.";

/// The transmit deferral is spent once per externally paced event, at the two
/// places that have one.
///
/// `site=` is deliberately absent from the `[SX_TX_DEFER]` line, so a capture
/// cannot tell the deferring sites apart — this can. What the test pins is the
/// invariant rather than a number: each site spends the bound once per entry,
/// and each is entered on an event a channel outside this loop wrote, not on
/// this loop's own cadence. A third site is therefore not forbidden; it is
/// required to arrive with that argument made, and this test says so by name
/// when one appears.
///
/// Until 2026-09-23 both sites were arms of one `select3` and slicing that
/// select was the whole test. The config arm then moved into `rx_window`,
/// because it belongs to every RX window the loop can park in and not only to
/// the idle one (a config pushed into the 20 s peer-turn yield was waited out,
/// and the cell that pushed it was skipped). So the two regions are now a
/// function and a select.
#[test]
fn the_transmit_deferral_is_spent_once_per_externally_paced_event() {
    let lora = nrf_source("lora.rs");

    // Region 1: `rx_window`, the loop's one RX window function. Its config arm
    // may defer; its reception arm may not — a reception is paced by the air
    // and by this loop, and a wait there would be the spacing delay the guard
    // is careful not to be.
    let win_start = sole_index(&lora, "async fn rx_window(");
    let win_end = win_start
        + lora[win_start..]
            .find("\n}\n")
            .expect("unterminated rx_window in leviculum-nrf/src/lora.rs");
    let window = &lora[win_start..win_end];
    assert!(
        window.contains("config_rx.ready_to_receive()"),
        "rx_window in leviculum-nrf/src/lora.rs no longer waits on \
         `config_rx.ready_to_receive()`; whether its deferring arm is entered on \
         an event paced from outside this loop has to be re-argued. \
         {DEFERRAL_HAND_CHECK}"
    );
    // The rule itself is `leviculum_rx_arming::window_entry`, host-tested in
    // that crate (leviculum#413); what this pins is that `rx_window` asks it
    // with the config channel's state and stands aside on its answer.
    assert!(
        window.contains("window_entry(!config_rx.is_empty())")
            && window.contains("WindowEntry::StandAside"),
        "rx_window arms a window even when a config is already waiting, so one \
         config push can now reach the deferral once per window instead of once. \
         {DEFERRAL_HAND_CHECK}"
    );
    let window_sites = transmit_deferral_sites(window);
    assert_eq!(
        window_sites.as_slice(),
        ["Config"],
        "the transmit-deferral sites in rx_window changed: expected [\"Config\"], \
         found {window_sites:?}. It is entered on the host's radio-config push, \
         paced by the host's console. {DEFERRAL_HAND_CHECK}"
    );
    let window_arm = window
        .find("Either::Second(()) =>")
        .expect("rx_window has no config arm to defer in");
    assert!(
        window_arm
            < window
                .find("disarm_rx_for_tx(")
                .expect("rx_window's deferral disappeared"),
        "rx_window defers before its config arm, i.e. on the reception path, \
         which this loop paces itself. {DEFERRAL_HAND_CHECK}"
    );

    // Region 2: the idle select, which now carries only what is peculiar to
    // that one window — the daemon's outgoing queue.
    let idle = &lora[sole_index(&lora, "let idle = select(")..];
    assert!(
        idle.contains("outgoing_rx.receive()"),
        "the idle select in leviculum-nrf/src/lora.rs no longer waits on \
         `outgoing_rx.receive()`; whether its deferring arm is entered on an \
         event paced from outside this loop has to be re-argued. \
         {DEFERRAL_HAND_CHECK}"
    );
    let idle_sites = transmit_deferral_sites(idle);
    assert_eq!(
        idle_sites.as_slice(),
        ["Select"],
        "the transmit-deferral sites in the idle select changed: expected \
         [\"Select\"], found {idle_sites:?}. That arm is the daemon's outgoing \
         queue, paced by the host's traffic. {DEFERRAL_HAND_CHECK}"
    );
    let idle_arm = idle
        .find("Either::Second(frame) =>")
        .expect("the idle select has no outgoing arm to defer in");
    assert!(
        idle_arm
            < idle
                .find("disarm_rx_for_tx(")
                .expect("the idle select's deferral disappeared"),
        "the idle select defers before its outgoing arm, i.e. on the reception \
         path, which this loop paces itself. {DEFERRAL_HAND_CHECK}"
    );

    // And nothing defers outside those two regions: a call in another helper,
    // or in the transmit path proper, is spent at a cadence this test never
    // looked at.
    let all = transmit_deferral_sites(&lora);
    assert_eq!(
        all.len(),
        window_sites.len() + idle_sites.len(),
        "leviculum-nrf/src/lora.rs defers a transmit outside rx_window's config \
         arm and the idle select's outgoing arm: {all:?} over the whole file \
         against {:?} inside them. {DEFERRAL_HAND_CHECK}",
        window_sites.len() + idle_sites.len()
    );
}

/// Nothing a prober needs to address the board is written on the gated path
/// (Codeberg #234).
///
/// `log_fmt` returns before it touches either the ring or the persistent tail
/// while `RUNTIME_DRAIN_OPEN` is unset (`leviculum-nrf/firmware/src/log.rs`), and the
/// gate opens on the first DTR-assert or after 30 s — both far later than the
/// boot sequence that registers the destinations. A one-shot `[IDENTITY]`
/// line written there is not late, it is gone: it never reaches the buffer
/// that survives the reset either, so the next boot's replay cannot bring it
/// back. That cost an external tester a full round trip, and the board's
/// `rnstransport.probe` hash is not obtainable any other way except by
/// catching an announce.
///
/// Two halves, and both are load-bearing. The lines are on the critical path,
/// so a reader attached inside the ring's memory still gets them; and the
/// banner repeats one of them, so a reader that attaches after the ring has
/// lapped gets it anyway. Either half alone leaves a window with no answer.
#[test]
fn the_identity_lines_are_not_dropped_by_the_runtime_gate() {
    for bin in ["bin/t114.rs", "bin/rak4631.rs", "bin/solarnode.rs"] {
        let src = board_boot_path(bin);
        assert!(
            !src.contains(r#"log_fmt("[IDENTITY] ""#),
            "leviculum-nrf/src/{bin} writes an [IDENTITY] line through the \
             runtime-gated log_fmt; pre-attach that line is dropped, not \
             delayed, and nothing re-emits it"
        );
        // The boot marker carries the same five identity bytes the display
        // shows, and it is the only line saying the node core came up at all;
        // the provenance pair says whether the hash the banner then prints is
        // the board's own or one made up this boot, which is what an operator
        // is sent to the debug port to read after a flash
        // (`docs/src/firmware/recovery.md`).
        for line in [
            "LNode started -- identity",
            "Identity loaded from flash",
            "No identity in flash",
        ] {
            let at = src
                .find(line)
                .unwrap_or_else(|| panic!("leviculum-nrf/src/{bin} no longer emits `{line}`"));
            let head = &src[..at];
            let critical = head.rfind("log_critical!");
            for gated in ["info!(", "log_fmt("] {
                assert!(
                    head.rfind(gated) < critical,
                    "leviculum-nrf/src/{bin} emits `{line}` through {gated}, which is \
                     the runtime-gated path"
                );
            }
        }
        // The rescue for a reader that attaches after the 8 KiB ring has
        // lapped: the periodic banner says it again. The loop is shared
        // (`leviculum_nrf::banner`, 911), so the binary has to run it and
        // the loop has to carry the identity line.
        assert!(
            src.contains("leviculum_nrf::banner::run("),
            "leviculum-nrf/src/{bin} no longer runs the banner loop, so a late \
             reader has only the boot window to catch the [IDENTITY] line in"
        );
    }
    assert!(
        nrf_source("banner.rs").contains("leviculum_firmware::identity::log_banner()"),
        "leviculum-nrf/src/banner.rs no longer repeats the [IDENTITY] banner, so a \
         late reader has only the boot window to catch it in"
    );
    let identity = firmware_source("identity.rs");
    assert!(
        identity.contains("crate::log_critical!"),
        "leviculum-nrf/firmware/src/identity.rs no longer emits its banner on the \
         boot-critical path"
    );
    // Every hash the banner publishes is on that one line, which is what
    // makes the per-BSP duplicates removable rather than merely redundant.
    assert!(
        identity.contains("[IDENTITY] identity={} probe={} lxmf={} lxmf_propagation={}"),
        "the [IDENTITY] banner lost a field; a hash that is no longer on it has \
         no other gate-free path to a reader"
    );
}

/// Codeberg #446: the BLE link-evidence lines are not dropped by the
/// runtime gate.
///
/// The reset census waits for `BLE_CONN_PARAMS conn=` on the board
/// that was reset (periculum `reset_loop.rs::LINK_NEEDLE`), and the
/// peripheral emits it exactly once per link, at the connected event.
/// On a T114 the debug port takes ~2.9 s to re-enumerate after a
/// reset, and the peer's central dial can land before that: in the
/// reset census of 2026-10-06 (attempts 3 and 17) the pocket connected
/// at T114-uptime ≈2.1 s and ≈2.4 s, the gate opened at 2939 ms and
/// 3229 ms, and the link's whole evidence — `BLE: connected`,
/// `BLE_CONN_PARAMS`, `BLE_CONN_PARAMS_REQ` — was counted into
/// RUNTIME_DROPPED while the link itself ran on and carried traffic
/// for the full 120 s window. Same failure shape as the identity
/// lines above (#234): pre-attach, a gated line is not late, it is
/// gone, and it never reaches the persistent tail either.
///
/// On the critical path the lines land in LOG_RING at the connected
/// event and are drained to the host when it attaches, which is how
/// every boot banner already reaches a capture. The cost is bounded by
/// the link rate, not the traffic rate — at most a handful of lines
/// per connection lifecycle — so the gate's purpose (the ring must not
/// be lapped by traffic before the host attaches) is untouched.
#[test]
fn the_ble_link_evidence_is_not_dropped_by_the_runtime_gate() {
    let columba = nrf_source("ble/columba.rs");
    // The two helpers behind `BLE_CONN_PARAMS` (both roles, open and
    // close) and `BLE_CONN_PARAMS_REQ` write through the gate-free
    // path, and through nothing else.
    for (marker, what) in [
        ("fn log_conn_params(", "the BLE_CONN_PARAMS helper"),
        (
            "fn ask_for_a_survivable_timeout(",
            "the BLE_CONN_PARAMS_REQ helper",
        ),
    ] {
        let body = body_after(&columba, "ble/columba.rs", marker, what);
        assert!(
            body.contains("log_fmt_critical("),
            "{what} in leviculum-nrf/src/ble/columba.rs no longer writes through \
             log_fmt_critical; emitted before DTR-assert its line is dropped, not \
             delayed, and the reset census waits 120 s for a line that no longer \
             exists (Codeberg #446)"
        );
        assert!(
            !body.contains("log_fmt("),
            "{what} in leviculum-nrf/src/ble/columba.rs writes through the \
             runtime-gated log_fmt again (Codeberg #446)"
        );
    }
    // The human-readable bracket of the same evidence. The byte shape
    // is pinned too: existing captures and the periculum fixtures carry
    // `[INFO] BLE: connected`, so the gate-free emission keeps the
    // `[INFO] ` prefix rather than switching to log_critical!'s
    // `[INFO!] `.
    for line in ["BLE: connected", "BLE: disconnected att_mtu="] {
        let at = columba
            .find(line)
            .unwrap_or_else(|| panic!("leviculum-nrf/src/ble/columba.rs no longer emits `{line}`"));
        let head = &columba[..at];
        let critical = head.rfind(r#"log_fmt_critical("[INFO] ""#);
        assert!(
            critical.is_some(),
            "leviculum-nrf/src/ble/columba.rs no longer emits `{line}` through \
             log_fmt_critical with the `[INFO] ` prefix the captures are grepped for"
        );
        for gated in ["info!(", "crate::log::log_fmt("] {
            assert!(
                head.rfind(gated) < critical,
                "leviculum-nrf/src/ble/columba.rs emits `{line}` through {gated}, \
                 which is the runtime-gated path: a central that connects before \
                 the debug port re-enumerates leaves no trace of the link \
                 (Codeberg #446)"
            );
        }
    }
}

/// Codeberg #296: the display and button bring-up lines are not dropped by
/// the runtime gate.
///
/// The boot smoke cells assert BLE, LoRa, battery, GNSS, heap and stack from
/// the board's own lines, and nothing asserted that the display or the button
/// came up: both report their bring-up within the first second or so of
/// boot, before the host attaches, and a gated line written then is not late,
/// it is gone (the same failure shape as the identity lines, #234, and the
/// BLE link evidence, #446). Each line is a handful of bytes once per boot,
/// so it belongs on the critical path like the banner.
///
/// Four emission sites, and every board reaches at least one `[DISP]` and
/// one `[BTN]` line: the Pocket's SSD1306 result (`display.rs`), the T114's
/// ST7789 result (`st7789.rs`), the button task's init line (`button.rs`),
/// and the declared absence a bin prints when it brings up no display or no
/// button, so a smoke cell can assert presence or absence on every board.
#[test]
fn the_display_and_button_bring_up_is_not_dropped_by_the_runtime_gate() {
    // The bring-up half of each task: everything before the render loop.
    for (file, marker, until, lines) in [
        (
            "display.rs",
            "pub async fn display_task(",
            "leviculum_firmware::display::run(",
            &[
                "OLED probe: {:?} at 0x{:02X}",
                "init failed: {:?}",
                "SSD1306 init ok",
                "SH1106 detected",
                "no OLED found on TWISPI0",
            ][..],
        ),
        (
            "st7789.rs",
            "pub async fn display_task(",
            "leviculum_firmware::display::run(",
            &["ST7789 blind init start", "ST7789 init complete"][..],
        ),
        (
            "button.rs",
            "pub async fn button_task(",
            "leviculum_firmware::button::run(",
            &["init pin=P{}.{:02} pull={} active={} debounce={}ms"][..],
        ),
    ] {
        let src = nrf_source(file);
        let body = body_after(&src, file, marker, "the bring-up task");
        let head = &body[..body
            .find(until)
            .unwrap_or_else(|| panic!("leviculum-nrf/src/{file}: `{until}` not found"))];
        for line in lines {
            assert!(
                head.contains(line),
                "leviculum-nrf/src/{file} no longer emits `{line}` during bring-up"
            );
        }
        assert!(
            head.contains("log_fmt_critical("),
            "leviculum-nrf/src/{file} no longer writes its bring-up through \
             log_fmt_critical (Codeberg #296)"
        );
        assert!(
            !head.contains("log_fmt(") && !head.contains("info!("),
            "leviculum-nrf/src/{file} writes a bring-up line through the runtime-gated \
             path; emitted before DTR-assert it is dropped, not delayed, and the \
             smoke cell cannot tell a dead display or button from a quiet one \
             (Codeberg #296)"
        );
    }

    // Declared absence: a bin that spawns no display task, or no button
    // task, says so once on the gate-free path.
    let none = |tag: &str| format!(r#"log_fmt_critical("{tag} ", format_args!("none"))"#);
    for (bin, tags) in [
        ("bin/solarnode.rs", &["[DISP]"][..]),
        ("bin/t1000e.rs", &["[DISP]"][..]),
    ] {
        let src = nrf_source(bin);
        for tag in tags {
            assert!(
                src.contains(&none(tag)),
                "leviculum-nrf/src/{bin} no longer declares `{tag} none` on the \
                 gate-free path (Codeberg #296)"
            );
        }
    }
    // Every board brings its button up, and no board that spawns the task
    // claims it has none (#459, 889). The debounce the init line names is
    // the shared constant, not a literal of the task's own.
    for bin in [
        "bin/t1000e.rs",
        "bin/rak4631.rs",
        "bin/t114.rs",
        "bin/solarnode.rs",
    ] {
        assert!(
            nrf_source(bin).contains("leviculum_nrf::button::init("),
            "leviculum-nrf/src/{bin} no longer spawns the button task (#459)"
        );
    }
    for bin in [
        "bin/t1000e.rs",
        "bin/rak4631.rs",
        "bin/t114.rs",
        "bin/solarnode.rs",
    ] {
        assert!(
            !nrf_source(bin).contains(&none("[BTN]")),
            "{bin} declares `[BTN] none` although it spawns the button task"
        );
    }
    assert!(
        nrf_source("button.rs").contains("BUTTON_DEBOUNCE_MS"),
        "button.rs no longer names the shared debounce constant in its init line"
    );
    // The RAK4631 declares no absence: its bare-module build, the one that
    // printed `[DISP] none` and `[BTN] none`, was retired (D5, Lew
    // 2026-10-28), and the Pocket image prints the SSD1306 driver's result
    // and the button task's init line. A `[DISP] none` back in its binary
    // would be the Pocket image claiming it has no display.
    assert!(
        !nrf_source("bin/rak4631.rs").contains(&none("[DISP]")),
        "bin/rak4631.rs declares `[DISP] none`; the Pocket image has a display \
         and the bare-module build is retired (D5)"
    );
}

/// Codeberg #398: `[TIME_SOURCE]` is the only thing a board says about its
/// clock, and the calendar can heal without passing any of the sites that
/// record what seated it.
///
/// The three seeding sites in each bin — GNSS, the host envelope, and the
/// propagation role's peer clock — each inject a value AND record the source
/// they injected. But `Transport::learn_emission_timebase` seats an anchor
/// too: it adopts an overheard announce's emission stamp and raises the
/// calendar to `TimeSource::Overheard` inside the core, with nothing at this
/// layer called when it does. The banner's atomic therefore kept the birth
/// state forever on a board that healed from traffic, and the rig capture of
/// 2026-09-14 read one identical word off three boards in two opposite clock
/// states: two T114s announcing their delivery destination on the configured
/// cadence, and a RAK4631 withholding its announce for want of a clock. That
/// pair is the one distinction the line exists to make.
///
/// The invariant is that the banner MIRRORS the node rather than collecting
/// notifications from the places that happen to know: each bin reads
/// `node.time_source()` beside the interface mirrors it already keeps, so an
/// anchor seated anywhere in the core reaches the banner within one loop
/// pass, and a fifth arm added to the core later needs no fourth seeding site
/// here. Pinned as source text for the same reason as every other invariant
/// in this file: the firmware cross-compiles to thumbv7em and cannot run a
/// host test.
#[test]
fn the_time_source_banner_mirrors_the_node_rather_than_the_seeding_sites() {
    for bin in ["bin/t114.rs", "bin/rak4631.rs", "bin/solarnode.rs"] {
        let src = board_boot_path(bin);
        assert!(
            src.contains("set_time_source(node.time_source())"),
            "leviculum-nrf/src/{bin} never mirrors node.time_source() into the \
             banner, so a calendar healed from an overheard announce keeps \
             reporting the birth state (Codeberg #398)"
        );
    }
    // The mirror is only a mirror if the store reports whether anything
    // moved; without that the bin has to choose between a [TIME_SOURCE] line
    // on every loop pass and none at all on the healed transition.
    // The store moved to the orchestration stratum's clock (#469, 958).
    let clock = firmware_source("clock.rs");
    assert!(
        clock.contains(
            "pub fn set_time_source(source: leviculum_core::transport::TimeSource) -> bool"
        ),
        "leviculum-nrf/firmware/src/clock.rs::set_time_source no longer reports \
         whether the recorded source changed, which is what lets the main-loop \
         mirror log the transition once (Codeberg #398)"
    );
}

/// The board's dial-refusal line (815): `[BLE ] BLE_DIAL_REFUSED
/// addr=<12 hex> lived_ms=<u64> fallback_in_ms=<u64>`, then the uptime stamp.
///
/// Returns `(addr, lived_ms, fallback_in_ms)`. All three are required: the
/// address says which peer refused, `lived_ms` is the evidence that it was a
/// refusal at all (gone inside the refusal window), and `fallback_in_ms` is
/// what the refusal cost the board.
fn parse_dial_refused(line: &str) -> Option<(u64, u64, u64)> {
    let idx = line.find("BLE_DIAL_REFUSED ")?;
    let rest = &line[idx + "BLE_DIAL_REFUSED ".len()..];
    let (mut addr, mut lived, mut fallback) = (None, None, None);
    for token in rest.split_whitespace() {
        let (key, value) = token.split_once('=')?;
        match key {
            "addr" if value.len() == 12 => addr = Some(u64::from_str_radix(value, 16).ok()?),
            "addr" => return None,
            "lived_ms" => lived = Some(value.parse().ok()?),
            "fallback_in_ms" => fallback = Some(value.parse().ok()?),
            _ => {} // the ` t=` stamp, and anything appended after it
        }
    }
    Some((addr?, lived?, fallback?))
}

/// The refusal line parses as the board writes it, and as lnsd's event of
/// the same name reads, `iface` aside: one grep reads both stacks.
#[test]
fn the_dial_refused_line_parses() {
    assert_eq!(
        parse_dial_refused(
            "[BLE ] BLE_DIAL_REFUSED addr=c00000009000 lived_ms=2990 fallback_in_ms=1280 t=48211"
        ),
        Some((0xc000_0000_9000, 2_990, 1_280))
    );
    assert_eq!(
        parse_dial_refused(
            "[INFO!] [PERSISTENT_LOG] [BLE ] BLE_DIAL_REFUSED addr=c00000009000 \
             lived_ms=12 fallback_in_ms=1280 t=9 t=2"
        ),
        Some((0xc000_0000_9000, 12, 1_280))
    );
    // A missing key is a parse failure, not a default.
    assert_eq!(
        parse_dial_refused("[BLE ] BLE_DIAL_REFUSED addr=c00000009000 lived_ms=2990 t=1"),
        None
    );
    assert_eq!(
        parse_dial_refused("[BLE ] BLE_DIAL_REFUSED addr=c00000009000 fallback_in_ms=1280 t=1"),
        None
    );
    // The address is the full six bytes, zero-padded, the shape lnsd's
    // `hex12` writes; a short one is a firmware that moved.
    assert_eq!(
        parse_dial_refused("[BLE ] BLE_DIAL_REFUSED addr=9000 lived_ms=1 fallback_in_ms=1280"),
        None
    );
}

/// The firmware still emits the refusal line, in the grammar the parser
/// above reads and with lnsd's keys. The format string lives in the host-
/// testable `ble-tx` crate (`DialRefusedLine`), the emission in the board's
/// BLE module; both are pinned, and the keys are checked against lnsd's own
/// catalogue entry so the two stacks cannot drift apart unnoticed.
#[test]
fn the_firmware_still_emits_the_dial_refused_line() {
    let admission = {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("leviculum-nrf/ble-tx/src/admission.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };
    const FORMAT: &str = "BLE_DIAL_REFUSED addr={:012x} lived_ms={} fallback_in_ms={}";
    assert!(
        admission.contains(&format!("\"{FORMAT}\"")),
        "the BLE_DIAL_REFUSED field order or spelling changed in \
         leviculum-nrf/ble-tx/src/admission.rs; parse_dial_refused above still \
         expects `{FORMAT}`"
    );
    let columba = nrf_source("ble/columba.rs");
    let body = body_after(
        &columba,
        "ble/columba.rs",
        "fn note_dial_refused(",
        "the refusal helper",
    );
    assert!(
        body.contains(r#""[BLE ] ""#) && body.contains("DialRefusedLine {"),
        "leviculum-nrf/src/ble/columba.rs::note_dial_refused no longer writes \
         DialRefusedLine under the `[BLE ] ` tag"
    );
    let lnsd = leviculum_std::event_log::EVENT_CATALOG
        .iter()
        .find(|e| e.name == "BLE_DIAL_REFUSED")
        .expect("lnsd's catalogue has a BLE_DIAL_REFUSED entry");
    let lnsd_keys: Vec<&str> = lnsd
        .required_keys
        .iter()
        .copied()
        .filter(|k| *k != "iface")
        .collect();
    assert_eq!(
        lnsd_keys,
        ["addr", "lived_ms", "fallback_in_ms"],
        "lnsd's BLE_DIAL_REFUSED keys, iface aside, are no longer the board's"
    );
}

/// Files whose `t=` in a log body is known and owned by another batch. Each
/// entry carries the reason; an entry outlives its reason by exactly one
/// review.
const BODY_STAMP_ALLOWED: &[(&str, &str)] = &[];

/// `src` with every comment blanked to spaces, string and char literals kept,
/// so a call named in a doc comment is not mistaken for a call.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    if b[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
                if i < b.len() {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 3,
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                i += 2;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).expect("blanking ASCII comments keeps UTF-8")
}

/// The string literals inside the balanced parentheses that open at `open`.
fn call_literals(src: &str, open: usize) -> Vec<String> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut literals = Vec::new();
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                literals.push(src[start..i].to_string());
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                while i + 1 < b.len() && b[i + 1] != b'\'' {
                    i += 1;
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    literals
}

/// Whether `literal` carries a standalone `t=` key (not `st=`, `sent=`).
fn has_stamp_key(literal: &str) -> bool {
    literal.match_indices("t=").any(|(at, _)| {
        literal[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    })
}

/// The `log_fmt*` lines that put a stamp in a firmware log body, as
/// `file:line` strings, comments excluded.
fn body_stamp_sites(rel: &str, src: &str) -> Vec<String> {
    // Every renderer and every macro that ends in one, plus `format_args!`:
    // a body built before the call (`retried!` in record_store.rs) reaches
    // the renderer as a value, so its literal is only visible there.
    const CALLS: &[&str] = &[
        "log_fmt(",
        "log_fmt_critical(",
        "log_fmt_volatile(",
        "info!(",
        "warn!(",
        "log_critical!(",
        "format_args!(",
    ];
    let code = strip_comments(src);
    let mut sites = Vec::new();
    for call in CALLS {
        for (at, _) in code.match_indices(call) {
            let preceded_by_ident = code[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if preceded_by_ident {
                continue;
            }
            let open = at + call.len() - 1;
            if call_literals(&code, open).iter().any(|l| has_stamp_key(l)) {
                let line = code[..at].matches('\n').count() + 1;
                sites.push(format!("{rel}:{line}"));
            }
        }
    }
    sites.sort();
    sites.dedup();
    sites
}

fn nrf_sources(dir: &Path, rel: &str, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().expect("entry name").to_string_lossy();
        let child = if rel.is_empty() {
            name.to_string()
        } else {
            format!("{rel}/{name}")
        };
        if path.is_dir() {
            nrf_sources(&path, &child, out);
        } else if name.ends_with(".rs") {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            out.push((child, src));
        }
    }
}

#[test]
fn the_stamp_detector_tells_a_body_stamp_from_its_neighbours() {
    let src = r#"
        // log_fmt("x", format_args!("t={}", 1));
        crate::log::log_fmt_critical("GNSS_POWER ", format_args!("{edge} t={now_ms}"));
        crate::log::log_fmt("[LORA] ", format_args!("st={} lt={} sent={}", a, b, c));
        info!("tx.t={} ')' {}", a, ')');
        warn!(
            "BTN_PRESS held_ms={} t={}",
            a, b
        );
    "#;
    let lines: Vec<String> = body_stamp_sites("fixture", src)
        .iter()
        .map(|site| site.replace("fixture:", "line "))
        .collect();
    assert_eq!(lines, ["line 3", "line 6"]);
}

/// The renderer stamps every line (`the_firmware_stamps_every_line_with_board_uptime`);
/// a body that stamps it again prints `t=` twice, and a parser that takes the
/// first `t=` and one that takes the last disagree the moment the two drift
/// apart (890: `GNSS_POWER ... t=603034 t=603034`).
#[test]
fn no_firmware_log_body_carries_its_own_stamp() {
    // Both strata log: the bind crate's `src/`, and the orchestration crate's
    // `firmware/src/`, whose files are named `firmware/<rel>` here.
    let nrf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("leviculum-nrf");
    let mut files = Vec::new();
    nrf_sources(&nrf.join("src"), "", &mut files);
    nrf_sources(&nrf.join("firmware/src"), "firmware", &mut files);
    assert!(
        files.iter().any(|(rel, _)| rel == "firmware/log.rs"),
        "the walk of leviculum-nrf/firmware/src found no log.rs; it is reading the wrong tree"
    );
    assert!(
        files.iter().any(|(rel, _)| rel == "lora.rs"),
        "the walk of leviculum-nrf/src found no lora.rs; it is reading the wrong tree"
    );
    let offending: Vec<String> = files
        .iter()
        .filter(|(rel, _)| !BODY_STAMP_ALLOWED.iter().any(|(f, _)| f == rel))
        .flat_map(|(rel, src)| body_stamp_sites(rel, src))
        .collect();
    assert!(
        offending.is_empty(),
        "these log bodies append their own t=, which the renderer already \
         stamps (leviculum-nrf/firmware/src/log.rs::fmt_into): {offending:?}"
    );
    for (allowed, reason) in BODY_STAMP_ALLOWED {
        let (_, src) = files
            .iter()
            .find(|(rel, _)| rel == allowed)
            .unwrap_or_else(|| panic!("allow-listed {allowed} no longer exists"));
        assert!(
            !body_stamp_sites(allowed, src).is_empty(),
            "{allowed} no longer stamps its own body; drop its BODY_STAMP_ALLOWED \
             entry ({reason})"
        );
    }
}
