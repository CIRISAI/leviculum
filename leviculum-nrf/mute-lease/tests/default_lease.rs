//! Where [`leviculum_mute_lease::DEFAULT_LEASE_S`] comes from.
//!
//! The default lease is the deadline a board falls back to when the host
//! sends `silence_lease_s = 0`, or sends the frame that predates the field —
//! which is every host in the tree today. So it is not a safety margin on top
//! of a number the harness sends; for now it IS the number, and if it is
//! short the corpus un-mutes boards in the middle of other cells'
//! measurements.
//!
//! What it has to cover is the gap between two consecutive silences of the
//! same board. `periculum::runner::silence_unused_lnode` pushes
//! `radio_silent = true` at every discovered board the current scenario did
//! not bind, **once per scenario**, so that gap is one scenario long — the
//! longest one in the corpus, plus the wall clock the harness spends around
//! it that no `timeout_secs` counts.
//!
//! The corpus is in another repository, so it cannot be read at test time the
//! way `leviculum-settle-budget` reads its airtime formula out of
//! `leviculum-core`. It is transcribed here instead, with the reading's date
//! and the command that produced it, and the assertions below are about the
//! relationship rather than about the individual numbers: a corpus that grows
//! a longer cell makes this file red, which is the point at which somebody
//! has to look at the default again.
//!
//! Read 2026-09-25 from `/home/lew/coding/periculum/hardware/*.toml` with
//!
//! ```text
//! for f in *.toml; do awk -v F="$f" '/^\[/{sec=$0} \
//!   /^timeout_secs *=/{ if (sec=="[test]") {gsub(/[^0-9]/,"",$3); print $3, F} }' "$f"; \
//!   done | sort -n | tail
//! ```
//!
//! Run: `cargo test -p leviculum-mute-lease --test default_lease -- --nocapture`

use leviculum_mute_lease::{MuteLease, DEFAULT_LEASE_S, LONGEST_HARDWARE_SCENARIO_S};

/// The top of the corpus's `[test] timeout_secs` distribution, longest
/// first. 129 files in `hardware/`, of which `KNOWN-REDS.toml` declares no
/// scenario.
const LONGEST_CELLS: &[(&str, u32)] = &[
    ("lora_lnode_path_soak", 4_200),
    ("lora_pn_board_offer_past_the_link", 3_600),
    ("lora_window_ab_pythonlike", 1_800),
    ("lora_window_ab_current", 1_800),
    ("lora_rncp_push_to_rust_50kb", 1_800),
    ("lora_pn_board_sync", 1_800),
    ("lora_lncp_push_to_python_50kb", 1_800),
    ("board_delivery_promise", 1_800),
    ("ble_pn_board_upload", 1_800),
];

/// The cell the constant names, and its configured timeout.
#[test]
fn the_longest_cell_is_the_lnode_path_soak_at_4200_s() {
    let (name, timeout_secs) = LONGEST_CELLS[0];
    println!(
        "LONGEST_HARDWARE_CELL name={name} timeout_secs={timeout_secs} \
         runner_up={} runner_up_timeout_secs={}",
        LONGEST_CELLS[1].0, LONGEST_CELLS[1].1
    );
    assert_eq!(name, "lora_lnode_path_soak");
    assert_eq!(timeout_secs, LONGEST_HARDWARE_SCENARIO_S);
    let measured_max = LONGEST_CELLS.iter().map(|&(_, t)| t).max().unwrap();
    assert_eq!(
        measured_max, LONGEST_HARDWARE_SCENARIO_S,
        "LONGEST_HARDWARE_SCENARIO_S must be the corpus maximum"
    );
}

/// The default covers that cell with half of it again in margin, for the
/// container start, the flash, the settle sleep and the teardown — none of
/// which is inside a cell's `timeout_secs`, and all of which falls between
/// two consecutive silences of the same board.
#[test]
fn the_default_lease_covers_the_longest_cell_with_margin() {
    let margin_s = u32::from(DEFAULT_LEASE_S) - LONGEST_HARDWARE_SCENARIO_S;
    println!(
        "DEFAULT_LEASE_S={DEFAULT_LEASE_S} longest_cell_s={LONGEST_HARDWARE_SCENARIO_S} \
         margin_s={margin_s}"
    );
    assert!(
        u32::from(DEFAULT_LEASE_S) > LONGEST_HARDWARE_SCENARIO_S,
        "a default lease that expires inside the longest cell un-mutes a \
         board in the middle of somebody else's measurement"
    );
    assert_eq!(margin_s, LONGEST_HARDWARE_SCENARIO_S / 2);
    assert_eq!(DEFAULT_LEASE_S, 6_300);
}

/// It has to fit the wire, which carries the lease as a `u16` of seconds —
/// so an explicit value can never be shorter than what a silent host gets by
/// saying nothing. `u16::MAX` is 18.2 hours, which bounds how long this
/// mechanism can ever be asked to hold, and 6300 s is well inside it.
#[test]
fn the_default_is_expressible_on_the_wire() {
    assert!(u32::from(DEFAULT_LEASE_S) <= u32::from(u16::MAX));
    println!(
        "WIRE_MAX_LEASE_S={} default_headroom_s={}",
        u16::MAX,
        u16::MAX - DEFAULT_LEASE_S
    );
}

/// **The control.** The corpus's own scale, against the lease a board would
/// actually run: every cell in the table must finish inside one default
/// lease, and the longest of them must NOT finish inside the corpus's second
/// -longest cell — which is what a default sized off the runner-up would
/// have given, and is the failure this file exists to notice.
#[test]
fn every_cell_finishes_inside_one_default_lease() {
    let mut lease = MuteLease::new();
    lease.grant(0, DEFAULT_LEASE_S);
    for &(name, timeout_secs) in LONGEST_CELLS {
        let cell_end_ms = u64::from(timeout_secs) * 1_000;
        assert!(
            lease.is_muted(cell_end_ms),
            "{name} runs {timeout_secs} s and the board un-mutes inside it"
        );
    }

    let mut undersized = MuteLease::new();
    undersized.grant(0, LONGEST_CELLS[1].1 as u16);
    assert!(
        !undersized.is_muted(u64::from(LONGEST_HARDWARE_SCENARIO_S) * 1_000),
        "a lease sized off the runner-up cell must be shown to be too short, \
         or this file's margin assertion is asserting nothing"
    );
}
