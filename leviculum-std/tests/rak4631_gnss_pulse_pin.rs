//! Codeberg #394: the RAK4631 image must claim no pulse-per-second pin.
//!
//! The RAK19026 VC baseboard schematic (RAKwireless, 11/26/2024, five
//! sheets, published as images on the WisMesh Base Board VC datasheet
//! page) settles two questions that our pin map had answered by
//! inference:
//!
//! * **Sheet 3, U6 (`ZOE-M8Q-0`).** Pin `C3`, `TIMEPULSE`, carries net
//!   `1PPS`, and the only thing on that net is `R39`, marked `0/NC` —
//!   a zero-ohm link that is *not fitted*. Behind it would sit `IO3`.
//!   So on an assembled board TIMEPULSE reaches no MCU pin at all, and
//!   the pin it would reach is not the one we were holding: RAK's own
//!   `WisCore_RAK4631_Board/variant.h` has `WB_IO3 = 21`, i.e. P0.21.
//!   The same sheet shows `STANDBY_GPS` through `R44` (`0/NC`, to
//!   `IO4`) and `RESET_GPS` through `R45` (`0/NC`, to `IO6`): the
//!   receiver's whole control side is depopulated, and only the UART
//!   links (`R43`, `0`, fitted) are there.
//! * **Sheet 4, U7 (`LIS3DH`).** Pin `11`, `INT1`, goes through `R53`,
//!   marked `0` — *fitted* — onto net `IO1`. `WB_IO1 = 17` in the same
//!   vendor header, so IO1 is P0.17: the pin the GNSS task was handed
//!   as `pps` is the accelerometer's interrupt output.
//!
//! Two consequences, and the second is why this is a test rather than a
//! comment. A pulse input that is wired to an accelerometer can never
//! see a pulse, so any time discipline built on it would measure
//! nothing. And `gnss_task` holds its pulse pin as `Input::new(pin,
//! Pull::Down)` for the life of the task, which parks a pull-down on the
//! interrupt line an accelerometer driver needs — and that part is the
//! candidate for the movement flag in the open announce-cadence
//! question, so the collision is about to become real rather than
//! theoretical.
//!
//! The pin numbers themselves cannot be tested from the host: nothing
//! here can read a schematic, and the board would not complain either,
//! because a quiet input looks exactly like a quiet pulse line. What can
//! be pinned is the decision — the RAK image claims no pulse pin, and
//! P0.17 appears nowhere in it — so that a later "add the missing PPS"
//! from some other board's variant header fails here instead of on a
//! bench nobody is watching.
//!
//! This lives in leviculum-std because leviculum-nrf cross-compiles to
//! thumbv7em and cannot run host tests; same reason as
//! `lnode_debug_log_format.rs`, which pins firmware source facts the
//! same way.

use std::path::{Path, PathBuf};

fn nrf_source(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("leviculum-nrf/src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The `GnssWiring { .. }` literal in `bin`, brace-balanced from the
/// struct name to its closing brace.
fn gnss_wiring_literal(bin: &str) -> String {
    let src = nrf_source(bin);
    let start = src
        .find("GnssWiring {")
        .unwrap_or_else(|| panic!("leviculum-nrf/src/{bin} no longer builds a GnssWiring"));
    let open = start + "GnssWiring ".len();
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return src[open..=open + i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("leviculum-nrf/src/{bin}: the GnssWiring literal is not brace-balanced")
}

/// The RAK's ZOE-M8Q has no reachable TIMEPULSE net, so the task gets no
/// pin to hold. `pps: None` is a supported wiring — the Solar Node has
/// passed it since #166 — and it is the one this board is entitled to.
#[test]
fn the_rak_gnss_task_is_handed_no_pulse_pin() {
    let wiring = gnss_wiring_literal("bin/rak4631.rs");
    assert!(
        wiring.contains("pps: None"),
        "leviculum-nrf/src/bin/rak4631.rs hands the GNSS task a pulse pin. The \
         RAK19026 VC leaves the ZOE-M8Q's TIMEPULSE on an unfitted 0 ohm link \
         (schematic sheet 3, R39 `0/NC`), so there is no pulse to receive on \
         any pin — and the pin this used to name, P0.17, is the LIS3DH's INT1 \
         (sheet 4, R53 `0`, fitted). See Codeberg #394.\nwiring literal:\n{wiring}"
    );
}

/// P0.17 is the accelerometer's interrupt output on this baseboard. The
/// image must not configure it for anything, not merely not for GNSS: a
/// pull-down held on someone else's output is the same defect whichever
/// task holds it.
///
/// Both ways this tree can claim a pin are checked, because the bug fixed
/// here used both: `peripherals::P0_17` behind a board alias, and
/// `p.P0_17` handed out of `embassy_nrf::init`'s `Peripherals`. Prose is
/// deliberately not matched — the board file explains at length which
/// part owns P0.17, and it has to be able to say so.
#[test]
fn the_rak_image_leaves_the_accelerometer_interrupt_line_alone() {
    for rel in ["bin/rak4631.rs", "boards/rak4631.rs"] {
        let src = nrf_source(rel);
        for claim in ["peripherals::P0_17", "p.P0_17"] {
            assert!(
                !src.contains(claim),
                "leviculum-nrf/src/{rel} claims P0.17 as `{claim}`. On the \
                 RAK19026 VC that pin is WB_IO1, which carries the LIS3DH's \
                 INT1 through a fitted 0 ohm link (schematic sheet 4, U7 pin \
                 11, R53). It is the accelerometer driver's to take, and \
                 nothing else may hold a pull on it. See Codeberg #394."
            );
        }
    }
}
