//! The T114's boot lines, pinned across its move onto the shared node loop
//! (#469 step 7, order 1085).
//!
//! The move puts about 700 lines of `leviculum-nrf/src/bin/t114.rs` into
//! `leviculum-nrf/src/node_loop.rs`, and the binary keeps only the board's
//! wiring between the skeleton's calls. Nothing a reader of the debug port
//! sees may change beyond the two decisions the order applies (D2, D7, Lew
//! 2026-10-28): the boot banner lines and their order, the `[STG]` stage
//! lines and the `BOOT_TRACE` phase stamps are what the rig cells and the
//! recovery doc key on, and the firmware cross-compiles to thumbv7em, so no
//! host test can run the boot itself.
//!
//! So the test reads the boot as TEXT, in the order the board runs it: the
//! binary's `main`, with every `node_loop::<step>(` call replaced by the
//! lines of that step's body, then the skeleton's tasks and helpers. From
//! that text it takes every line a board writes (`log_critical!`, `info!`,
//! `warn!`, `log_fmt`/`log_fmt_critical` with their prefix, every call of a
//! `log_*` helper, every `boot_trace` `Phase::`) in order, and compares the
//! sequence with [`PINNED_PRE_1085`]: the same extraction over the binary as
//! it stood at 40836451, before the move, with the two decided changes
//! applied by name in [`expected`].

#[path = "support/boot_lines.rs"]
mod boot_lines;

use boot_lines::{lines_written, mismatch, strip_comments, tree};

/// Every line `bin/t114.rs` wrote at 40836451, in source order, as the
/// extraction below spells it. Generated once from that file; a change here
/// is a change to what the T114 says at boot and needs its own decision.
const PINNED_PRE_1085: &[&str] = &[
    "phase PersistRead",
    "phase UsbUp",
    "log_critical leviculum T114 booting",
    "log_critical [FW_BUILD] {}",
    "log_critical [TIME_SOURCE] source={}",
    "call log_boot_diagnostics",
    "log_critical Identity loaded from flash",
    "log_critical No identity in flash, generating new",
    "call heap_census::log_budget_and_assert",
    "log_critical [TIME_SOURCE] source={}",
    "call mgmt::log_banner",
    "info [BOOT] path_table_initial_len={}",
    "info Identity saved to flash",
    "log_critical LNode started -- identity: {:02X}{:02X}{:02X}{:02X}{:02X}",
    "log_fmt_critical [QSPI] NONE board=t114 reason=not-fitted-see-boards-t114-rs",
    "log_critical [STG] lora-init",
    "info SX1262 ready",
    "log_fmt [RADIO] persisted freq={} bw={} sf={} cr={} pwr={}",
    "log_fmt [RADIO] default eu_medium",
    "phase LoraTask",
    "call media::log_carrier_held_down",
    "phase LoraSkipped",
    "log_critical [STG] ble-init",
    "info BLE ready",
    "call media::log_banner",
    "call name::log_banner",
    "log_critical [STG] display-spawn",
    "info display task spawned (ST7789 blind-drive)",
    "info gnss task spawned (L76K)",
    "info battery task spawned",
    "info heap u={} f={}",
    "call log_stack",
    "log_critical PN role=off reason=identity-underivable",
    "log_critical [TELEMETRY] target=00000000 state=off reason=no-delivery-destination",
    "call reporter.log_banner",
    "log_critical [TIME_SEED_REFUSED] source=gnss reason=watch_capacity",
    "log_critical [STG] main-loop",
    "phase MainLoop",
    "call events::log_events",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED] source=gnss unix={}",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED_REFUSED] source=gnss unix={}",
    "log_critical [TIME_SEED] source=host unix={}",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED_REFUSED] source=host unix={}",
    "call reporter.log_banner",
    "log_critical [TELEMETRY] fixed-position set lat_e6={} lon_e6={} alt_e2={} alt_present={}",
    "log_critical [TELEMETRY] fixed-position cleared",
    "log_critical [ANNOUNCE] withheld reason=no-clock",
    "log_critical [ANNOUNCE] sent dst={:02x}{:02x}{:02x}{:02x} reason=host",
    "info SER RX {} bytes",
    "info SER RX -> {} actions",
    "info LORA RX -> {} actions",
    "info BLE RX -> {} actions",
    "info BLE peer lost, {} paths culled",
    "info BLE peer up, {} pull actions",
    "info timeout: {} actions",
    "info [BOOT] path_table_initial_len={}",
];

/// The pinned sequence with the order's two decisions applied.
fn expected() -> Vec<String> {
    let mut lines: Vec<String> = PINNED_PRE_1085.iter().map(|s| s.to_string()).collect();
    // D2: every GNSS board prints the `[GNSS_PRESENCE] state=detecting`
    // anchor from `gnss.rs`, after the first `[TIME_SOURCE]` and before the
    // boot diagnostics, where the RAK4631 has always printed it.
    let first_time_source = lines
        .iter()
        .position(|l| l == "log_critical [TIME_SOURCE] source={}")
        .expect("pinned list has a [TIME_SOURCE] line");
    lines.insert(
        first_time_source + 1,
        "call gnss::log_detecting".to_string(),
    );
    // D7: the spawn lines say `<task> task spawned (<module>)`, the module
    // named by the board file; "blind-drive" was the T114's alone.
    let display = lines
        .iter()
        .position(|l| l == "info display task spawned (ST7789 blind-drive)")
        .expect("pinned list has the display spawn line");
    lines[display] = "info display task spawned (ST7789)".to_string();
    lines
}

fn sources() -> (String, String, String) {
    (
        tree("leviculum-nrf/src/bin/t114.rs"),
        tree("leviculum-nrf/src/node_loop.rs"),
        tree("leviculum-nrf/src/boards/t114.rs"),
    )
}

#[test]
fn the_t114_boot_lines_are_the_pre_move_lines_plus_d2_and_d7() {
    let (bin, skeleton, board) = sources();
    let got = boot_lines::boot_lines(&bin, &skeleton, &board, "t114");
    if let Some(diff) = mismatch(&got, &expected()) {
        panic!(
            "the T114's boot lines moved (order 1085 pins them): {diff}\nfull sequence:\n{}",
            got.join("\n")
        );
    }
}

/// Positive control: one line dropped from the skeleton, one step call
/// dropped from the binary, and one line moved, each turn the comparison
/// red.
#[test]
fn a_dropped_line_turns_the_pin_red() {
    let (bin, skeleton, board) = sources();
    let drop_line = |src: &str, needle: &str| -> String {
        assert!(src.contains(needle), "control needle {needle:?} is gone");
        src.lines()
            .filter(|l| !l.contains(needle))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let want = expected();
    let no_stage = drop_line(&skeleton, r#"log_critical!("[STG] ble-init");"#);
    assert!(
        mismatch(
            &boot_lines::boot_lines(&bin, &no_stage, &board, "t114"),
            &want
        )
        .is_some(),
        "dropping `[STG] ble-init` from the skeleton left the pin green"
    );
    let no_banner = drop_line(&bin, "node_loop::boot_banner(");
    assert!(
        mismatch(
            &boot_lines::boot_lines(&no_banner, &skeleton, &board, "t114"),
            &want
        )
        .is_some(),
        "dropping the boot banner step from the binary left the pin green"
    );
    // Order, not only the set: the stage line moved behind the line it
    // brackets keeps every line and still turns the pin red.
    let stage = r#"    log_critical!("[STG] lora-init");"#;
    let ready = r#"    info!("SX1262 ready");"#;
    assert!(
        bin.contains(stage) && bin.contains(ready),
        "control lines are gone"
    );
    let reordered = bin
        .replacen(stage, "", 1)
        .replacen(ready, &format!("{ready}\n{stage}"), 1);
    assert!(
        mismatch(
            &boot_lines::boot_lines(&reordered, &skeleton, &board, "t114"),
            &want
        )
        .is_some(),
        "moving `[STG] lora-init` behind `SX1262 ready` left the pin green"
    );
}

/// The extraction itself, on the pre-move binary's shape: a line inside a
/// comment is not a line, a `log_fmt` keeps its prefix, a definition is not
/// a call.
#[test]
fn the_extraction_reads_what_a_board_writes() {
    let src = "// log_critical!(\"commented\");\nlog_critical!(\"a {}\", x);\nleviculum_nrf::log::log_fmt(\"[R] \", format_args!(\"b={}\", y));\nfn log_banner() {}\nmedia::log_banner(z);\nboot_trace::phase(Phase::UsbUp);\n";
    let got: Vec<String> = lines_written(&strip_comments(src))
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    assert_eq!(
        got,
        [
            "log_critical a {}",
            "log_fmt [R] b={}",
            "call media::log_banner",
            "phase UsbUp"
        ]
    );
}
