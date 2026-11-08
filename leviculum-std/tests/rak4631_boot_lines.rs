//! The RAK4631's boot lines, pinned across its move onto the shared node
//! loop (#469 step 7, order 1572).
//!
//! The T114 moved first (order 1085, `t114_boot_lines`); this is the same
//! pin for the WisMesh Pocket V2's binary, read by the same extraction
//! (`support/boot_lines.rs`). The RAK carried the decisions the other three
//! boards did not, so here they all apply (Lew, 2026-10-28, all as the
//! node-loop diff recommended): D1 the `[STG]` stage lines, D2 the GNSS
//! presence anchor from `gnss.rs`, D5 the bare-module build retired, D6 the
//! `ACT` lines of the timeout arm gone, D7 the spawn line wording, D8 the
//! T114's order of the GNSS seed receiver and the GNSS arm. D3 (the boot
//! blink) and D4 (the activity pulses through the feedback sinks) live in
//! `led.rs` and `feedback.rs` and write no line of the binary's.
//!
//! [`PINNED_PRE_1572`] is the extraction over `bin/rak4631.rs` as it stood
//! at 0db76fa8, before the move; [`expected`] applies the decisions by
//! name, each with the row of `docs/drafts/node-loop-diff-2026-10.md` it
//! answers. The firmware cross-compiles to thumbv7em, so no host test runs
//! the boot itself: this reads it as text.

#[path = "support/boot_lines.rs"]
mod boot_lines;

use boot_lines::{mismatch, tree};

/// Every line `bin/rak4631.rs` wrote at 0db76fa8, in source order, as the
/// extraction spells it, both cfg arms of the bare-module split included.
/// Generated once from that file; a change here is a change to what the
/// RAK4631 says at boot and needs its own decision.
const PINNED_PRE_1572: &[&str] = &[
    "phase PersistRead",
    "phase UsbUp",
    "log_critical leviculum RAK4631 booting",
    "log_critical [FW_BUILD] {}",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [GNSS_PRESENCE] state=detecting baud={}",
    "call log_boot_diagnostics",
    "log_critical Identity loaded from flash",
    "log_critical No identity in flash, generating new",
    "call heap_census::log_budget_and_assert",
    "log_critical [TIME_SOURCE] source={}",
    "call mgmt::log_banner",
    "info [BOOT] path_table_initial_len={}",
    "info Identity saved to flash",
    "log_critical LNode started -- identity: {:02X}{:02X}{:02X}{:02X}{:02X}",
    "log_fmt_critical [QSPI] NONE board=rak4631 reason=no-onboard-flash-see-boards-rak4631-rs",
    "info SX1262 ready",
    "log_fmt [RADIO] persisted freq={} bw={} sf={} cr={} pwr={}",
    "log_fmt [RADIO] default eu_medium",
    "phase LoraTask",
    "call media::log_carrier_held_down",
    "phase LoraSkipped",
    "info BLE ready",
    "call media::log_banner",
    "call name::log_banner",
    "info display task spawned",
    "info button task spawned",
    "log_fmt_critical [DISP] none",
    "log_fmt_critical [BTN] none",
    "info gnss task spawned",
    "info battery task spawned",
    "info heap u={} f={}",
    "call log_stack",
    "log_critical [TIME_SEED_REFUSED] source=gnss reason=watch_capacity",
    "log_critical PN role=off reason=identity-underivable",
    "log_critical [TELEMETRY] target=00000000 state=off reason=no-delivery-destination",
    "call reporter.log_banner",
    "phase MainLoop",
    "call events::log_events",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED] source=host unix={}",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED_REFUSED] source=host unix={}",
    "call reporter.log_banner",
    "log_critical [TELEMETRY] fixed-position set lat_e6={} lon_e6={} alt_e2={} alt_present={}",
    "log_critical [TELEMETRY] fixed-position cleared",
    "log_critical [ANNOUNCE] withheld reason=no-clock",
    "log_critical [ANNOUNCE] sent dst={:02x}{:02x}{:02x}{:02x} reason=host",
    "log_critical [TIME_SEED] source=gnss unix={}",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED_REFUSED] source=gnss unix={}",
    "info SER RX {} bytes",
    "info SER RX -> {} actions",
    "info LORA RX -> {} actions",
    "info BLE RX -> {} actions",
    "info BLE peer lost, {} paths culled",
    "info BLE peer up, {} pull actions",
    "info timeout: {} actions",
    "info ACT SendPacket iface={} len={} routed={}",
    "info ACT Broadcast excl={} excl_link={:02x}{:02x}{:02x}{:02x} len={}",
    "info ACT Broadcast excl={} excl_link=- len={}",
    "info [BOOT] path_table_initial_len={}",
];

fn at(lines: &[String], line: &str) -> usize {
    lines
        .iter()
        .position(|l| l == line)
        .unwrap_or_else(|| panic!("pinned list has no {line:?}"))
}

fn remove(lines: &mut Vec<String>, line: &str) {
    let i = at(lines, line);
    lines.remove(i);
}

fn insert_before(lines: &mut Vec<String>, before: &str, line: &str) {
    let i = at(lines, before);
    lines.insert(i, line.to_string());
}

/// The pinned sequence with Lew's decisions of 2026-10-28 applied.
fn expected() -> Vec<String> {
    let mut lines: Vec<String> = PINNED_PRE_1572.iter().map(|s| s.to_string()).collect();

    // D2: the anchor the RAK always printed, now from `gnss.rs` with the
    // board's baud, as on every GNSS board. Same words, same place.
    let anchor = at(
        &lines,
        "log_critical [GNSS_PRESENCE] state=detecting baud={}",
    );
    lines[anchor] = "call gnss::log_detecting".to_string();

    // D1: the stage lines every other board prints, and the display task's
    // own stage where a display is spawned.
    insert_before(
        &mut lines,
        "info SX1262 ready",
        "log_critical [STG] lora-init",
    );
    insert_before(&mut lines, "info BLE ready", "log_critical [STG] ble-init");
    insert_before(
        &mut lines,
        "info display task spawned",
        "log_critical [STG] display-spawn",
    );
    insert_before(&mut lines, "phase MainLoop", "log_critical [STG] main-loop");

    // D7: `<task> task spawned (<module>)` with the module from the board
    // file, and no button line no other board prints.
    let display = at(&lines, "info display task spawned");
    lines[display] = "info display task spawned (SSD1306)".to_string();
    let gnss = at(&lines, "info gnss task spawned");
    lines[gnss] = "info gnss task spawned (ZOE-M8Q)".to_string();
    remove(&mut lines, "info button task spawned");

    // D5: the bare-module build is retired, and with it the two absence
    // lines only it printed. The Pocket image prints the display driver's
    // result and the button task's `[BTN] init` instead, as it did.
    remove(&mut lines, "log_fmt_critical [DISP] none");
    remove(&mut lines, "log_fmt_critical [BTN] none");

    // D6: the #422 diagnostic of the timeout arm goes.
    remove(&mut lines, "info ACT SendPacket iface={} len={} routed={}");
    remove(
        &mut lines,
        "info ACT Broadcast excl={} excl_link={:02x}{:02x}{:02x}{:02x} len={}",
    );
    remove(&mut lines, "info ACT Broadcast excl={} excl_link=- len={}");

    // D8: the T114's order. The GNSS seed receiver is taken after the
    // telemetry banner, not before the PN role ...
    let refused = "log_critical [TIME_SEED_REFUSED] source=gnss reason=watch_capacity";
    remove(&mut lines, refused);
    let banner = at(&lines, "call reporter.log_banner");
    lines.insert(banner + 1, refused.to_string());
    // ... and the GNSS arm is the loop's second, before the host's wall
    // time, not after the host announce.
    let seed = at(&lines, "log_critical [TIME_SEED] source=gnss unix={}");
    let arm: Vec<String> = lines.drain(seed..seed + 3).collect();
    assert_eq!(
        arm.last().map(String::as_str),
        Some("log_critical [TIME_SEED_REFUSED] source=gnss unix={}"),
        "the pinned GNSS arm is not three lines"
    );
    let events = at(&lines, "call events::log_events");
    for (k, line) in arm.into_iter().enumerate() {
        lines.insert(events + 2 + k, line);
    }
    lines
}

fn sources() -> (String, String, String) {
    (
        tree("leviculum-nrf/src/bin/rak4631.rs"),
        tree("leviculum-nrf/src/node_loop.rs"),
        tree("leviculum-nrf/src/boards/rak4631.rs"),
    )
}

#[test]
fn the_rak4631_boot_lines_are_the_pre_move_lines_with_the_decisions_applied() {
    let (bin, skeleton, board) = sources();
    let got = boot_lines::boot_lines(&bin, &skeleton, &board, "rak4631");
    if let Some(diff) = mismatch(&got, &expected()) {
        panic!(
            "the RAK4631's boot lines moved (order 1572 pins them): {diff}\nfull sequence:\n{}",
            got.join("\n")
        );
    }
}

/// With the decisions applied, the RAK writes what the T114 writes but for
/// its board facts: the boot line, the QSPI statement and the two module
/// names. Nothing above the chip is the RAK's own any more.
#[test]
fn the_rak4631_writes_the_t114s_lines_but_for_its_board_facts() {
    let (bin, skeleton, board) = sources();
    let rak = boot_lines::boot_lines(&bin, &skeleton, &board, "rak4631");
    let t114 = boot_lines::boot_lines(
        &tree("leviculum-nrf/src/bin/t114.rs"),
        &skeleton,
        &tree("leviculum-nrf/src/boards/t114.rs"),
        "t114",
    );
    let fact = |l: &str| {
        l.contains(" booting")
            || l.starts_with("log_fmt_critical [QSPI] NONE")
            || l.starts_with("info display task spawned (")
            || l.starts_with("info gnss task spawned (")
    };
    let strip = |v: &[String]| -> Vec<String> { v.iter().filter(|l| !fact(l)).cloned().collect() };
    if let Some(diff) = mismatch(&strip(&rak), &strip(&t114)) {
        panic!("the RAK4631 and the T114 differ beyond their board facts: {diff}");
    }
}

/// Positive control: one line dropped from the skeleton, one step call
/// dropped from the binary, and one line moved, each turn the comparison
/// red.
#[test]
fn a_dropped_or_moved_line_turns_the_pin_red() {
    let (bin, skeleton, board) = sources();
    let drop_line = |src: &str, needle: &str| -> String {
        assert!(src.contains(needle), "control needle {needle:?} is gone");
        src.lines()
            .filter(|l| !l.contains(needle))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let want = expected();
    let no_stage = drop_line(&skeleton, r#"log_critical!("[STG] main-loop");"#);
    assert!(
        mismatch(
            &boot_lines::boot_lines(&bin, &no_stage, &board, "rak4631"),
            &want
        )
        .is_some(),
        "dropping `[STG] main-loop` from the skeleton left the pin green"
    );
    let no_post_init = drop_line(&bin, "node_loop::log_post_init(");
    assert!(
        mismatch(
            &boot_lines::boot_lines(&no_post_init, &skeleton, &board, "rak4631"),
            &want
        )
        .is_some(),
        "dropping the post-init step from the binary left the pin green"
    );
    // Order, not only the set.
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
            &boot_lines::boot_lines(&reordered, &skeleton, &board, "rak4631"),
            &want
        )
        .is_some(),
        "moving `[STG] lora-init` behind `SX1262 ready` left the pin green"
    );
}
