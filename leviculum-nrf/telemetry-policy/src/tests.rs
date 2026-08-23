//! Behaviour tests for the telemetry send policy (Codeberg #236).
//!
//! Each test names one rule from `docs/src/concepts/telemetry.md` or from
//! the #236 UX decisions and exercises it alone. The state machine has no
//! I/O, so every one of them is a few microseconds of arithmetic — which
//! is the whole point of the policy being a crate and not a branch inside
//! the firmware's main loop.

use super::*;

/// Somewhere in Hamburg, HDOP 1.2 — a fix any profile accepts.
fn good_fix() -> Fix {
    Fix {
        latitude_e6: 53_551_086,
        longitude_e6: 9_993_682,
        hdop_e2: Some(120),
    }
}

/// Move `metres` due north of `from` (latitude only, so the expected
/// distance is exact under the model).
fn north_of(from: Fix, metres: i64) -> Fix {
    Fix {
        latitude_e6: from.latitude_e6 + (metres * 1_000_000 / METRES_PER_DEGREE_LAT) as i32,
        ..from
    }
}

/// A policy already past the target dance and past its settle window,
/// with one report on the clock at `t`.
fn ready(profile: Profile, t: u64) -> SendPolicy {
    let mut p = SendPolicy::new();
    p.set_target(profile, true);
    assert_eq!(p.poll(t, Some(good_fix())), Some(ReportReason::Immediate));
    p.note_sent(t, Some(good_fix()));
    p
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

#[test]
fn station_is_the_default_profile() {
    assert_eq!(Profile::DEFAULT, Profile::Station);
    assert_eq!(SendPolicy::new().profile(), Profile::Station);
}

#[test]
fn profile_ids_match_the_envelope_allocation() {
    assert_eq!(Profile::Tracker.to_wire(), 0x01);
    assert_eq!(Profile::Station.to_wire(), 0x02);
    assert_eq!(Profile::from_wire(0x01), Some(Profile::Tracker));
    assert_eq!(Profile::from_wire(0x02), Some(Profile::Station));
    // 0x00 is the clear encoding, not a profile. Pinned to the literal,
    // not to the constant: this crate cannot depend on
    // `leviculum_core::envelope`, so the two allocations are held
    // together by this assertion here and a compile-time one in the
    // firmware glue that sees both.
    assert_eq!(PROFILE_ID_OFF, 0x00);
    assert_eq!(PROFILE_ID_TRACKER, 0x01);
    assert_eq!(PROFILE_ID_STATION, 0x02);
    assert_eq!(Profile::from_wire(PROFILE_ID_OFF), None);
    assert_eq!(Profile::from_wire(0x7F), None);
}

#[test]
fn the_station_profile_has_no_movement_path() {
    // What makes a station a station: distance never justifies a report.
    assert_eq!(PolicyParams::STATION.min_distance_m, 0);
    assert_eq!(
        PolicyParams::STATION.min_interval_ms,
        PolicyParams::STATION.max_interval_ms
    );
}

#[test]
fn setting_a_target_loads_that_profiles_parameters() {
    let mut p = SendPolicy::new();
    p.set_target(Profile::Tracker, true);
    assert_eq!(p.params(), PolicyParams::TRACKER);
    p.set_target(Profile::Station, true);
    assert_eq!(p.params(), PolicyParams::STATION);
}

// ---------------------------------------------------------------------------
// Target lifecycle — the hash-only UX decision
// ---------------------------------------------------------------------------

#[test]
fn no_target_is_the_default_and_sends_nothing() {
    let mut p = SendPolicy::new();
    assert_eq!(p.state(), TargetState::Off);
    assert_eq!(p.poll(0, Some(good_fix())), None);
    assert_eq!(p.poll(u64::MAX / 2, Some(good_fix())), None);
}

#[test]
fn a_hash_only_target_waits_for_the_key_and_says_so() {
    let mut p = SendPolicy::new();
    assert_eq!(
        p.set_target(Profile::Station, false),
        TargetState::AwaitingKey
    );
    assert_eq!(p.state().as_str(), "awaiting-key");
    // Nothing can be encrypted, so nothing is due — at any time.
    assert_eq!(p.poll(0, Some(good_fix())), None);
    assert_eq!(p.poll(10 * 60 * 60_000, Some(good_fix())), None);
}

#[test]
fn the_immediate_report_fires_when_the_key_arrives_not_when_the_target_is_set() {
    let mut p = SendPolicy::new();
    p.set_target(Profile::Station, false);
    assert_eq!(p.poll(1_000, Some(good_fix())), None);

    assert!(p.note_key_available());
    assert_eq!(p.state(), TargetState::Ready);
    assert_eq!(
        p.poll(1_100, Some(good_fix())),
        Some(ReportReason::Immediate)
    );
}

#[test]
fn a_target_set_with_a_known_key_is_ready_at_once() {
    let mut p = SendPolicy::new();
    assert_eq!(p.set_target(Profile::Station, true), TargetState::Ready);
    assert_eq!(p.poll(0, None), Some(ReportReason::Immediate));
}

#[test]
fn the_immediate_report_survives_until_it_is_actually_sent() {
    // A node with no path must retry, not go quiet for an hour.
    let mut p = ready(Profile::Station, 0);
    p.set_target(Profile::Station, true);
    for t in [0, 5_000, 60_000, 600_000] {
        assert_eq!(p.poll(t, None), Some(ReportReason::Immediate), "at t={t}");
    }
    p.note_sent(600_000, None);
    assert_eq!(p.poll(600_001, None), None);
}

#[test]
fn key_arrival_on_an_already_ready_target_changes_nothing() {
    let mut p = ready(Profile::Station, 0);
    assert!(!p.note_key_available());
    assert_eq!(p.poll(1_000, Some(good_fix())), None);
}

#[test]
fn clearing_the_target_switches_telemetry_off() {
    let mut p = ready(Profile::Tracker, 0);
    p.clear_target();
    assert_eq!(p.state(), TargetState::Off);
    assert_eq!(p.state().as_str(), "off");
    // Not even the heartbeat, which is the whole point of "the target is
    // the switch".
    assert_eq!(p.poll(10 * 60 * 60_000, Some(good_fix())), None);
}

#[test]
fn a_cleared_target_does_not_leave_cadence_state_for_the_next_one() {
    let mut p = ready(Profile::Tracker, 0);
    p.note_sent(0, Some(good_fix()));
    p.clear_target();
    // A new target, key already known: the first thing owed is the
    // immediate report, not a movement report against the old
    // recipient's reference position.
    p.set_target(Profile::Tracker, true);
    assert_eq!(
        p.poll(1_000, Some(north_of(good_fix(), 500))),
        Some(ReportReason::Immediate)
    );
}

#[test]
fn losing_the_key_returns_to_awaiting_and_disarms() {
    let mut p = ready(Profile::Station, 0);
    p.set_target(Profile::Station, true);
    assert!(p.note_key_lost());
    assert_eq!(p.state(), TargetState::AwaitingKey);
    assert_eq!(p.poll(1_000, Some(good_fix())), None);
    // And it re-arms when the key comes back.
    assert!(p.note_key_available());
    assert_eq!(
        p.poll(2_000, Some(good_fix())),
        Some(ReportReason::Immediate)
    );
}

// ---------------------------------------------------------------------------
// Heartbeat — the maximum interval
// ---------------------------------------------------------------------------

#[test]
fn the_heartbeat_fires_exactly_at_the_maximum_interval() {
    let max = PolicyParams::STATION.max_interval_ms;
    let mut p = ready(Profile::Station, 0);
    assert_eq!(p.poll(max - 1, None), None);
    assert_eq!(p.poll(max, None), Some(ReportReason::Heartbeat));
}

#[test]
fn the_heartbeat_fires_without_any_position() {
    // "no fix, no position" does not mean "no fix, no message": a
    // heartbeat still carries battery and time.
    let mut p = ready(Profile::Station, 0);
    let max = PolicyParams::STATION.max_interval_ms;
    assert_eq!(p.poll(max, None), Some(ReportReason::Heartbeat));
}

#[test]
fn the_heartbeat_restarts_from_the_report_that_was_actually_sent() {
    let max = PolicyParams::STATION.max_interval_ms;
    let mut p = ready(Profile::Station, 0);
    assert_eq!(p.poll(max, None), Some(ReportReason::Heartbeat));
    // Radio refused it: the cadence must not advance.
    assert_eq!(p.poll(max + 1_000, None), Some(ReportReason::Heartbeat));
    p.note_sent(max + 1_000, None);
    assert_eq!(p.poll(max + 2_000, None), None);
    assert_eq!(p.poll(2 * max + 1_000, None), Some(ReportReason::Heartbeat));
}

// ---------------------------------------------------------------------------
// Movement — distance and the minimum interval
// ---------------------------------------------------------------------------

#[test]
fn a_tracker_reports_after_moving_far_enough() {
    let mut p = ready(Profile::Tracker, 0);
    let settle = PolicyParams::TRACKER.settle_ms;
    let t = settle + PolicyParams::TRACKER.min_interval_ms;
    let moved = north_of(good_fix(), 120);
    assert_eq!(p.poll(t, Some(moved)), Some(ReportReason::Movement));
}

#[test]
fn a_tracker_that_has_not_moved_far_enough_stays_quiet() {
    let mut p = ready(Profile::Tracker, 0);
    let t = PolicyParams::TRACKER.settle_ms + PolicyParams::TRACKER.min_interval_ms;
    // 20 m against a 50 m gate.
    assert_eq!(p.poll(t, Some(north_of(good_fix(), 20))), None);
}

#[test]
fn the_minimum_interval_holds_a_movement_report_back() {
    let mut p = ready(Profile::Tracker, 0);
    // Settle out of the way, so this test is about the interval floor
    // alone: the tracker preset happens to set the two to the same 60 s
    // and a test that cannot tell them apart proves neither.
    p.set_params(PolicyParams {
        settle_ms: 0,
        ..PolicyParams::TRACKER
    });
    let far = north_of(good_fix(), 500);
    let min = PolicyParams::TRACKER.min_interval_ms;
    assert_eq!(p.poll(min - 1, Some(far)), None);
    assert_eq!(p.poll(min, Some(far)), Some(ReportReason::Movement));
}

#[test]
fn distance_is_measured_against_the_last_reported_position_not_the_last_seen_one() {
    let mut p = ready(Profile::Tracker, 0);
    let base = PolicyParams::TRACKER.settle_ms + PolicyParams::TRACKER.min_interval_ms;
    // Crawl 20 m at a time: no single step passes the gate, but the
    // third step is 60 m from the reported position and must report.
    assert_eq!(p.poll(base, Some(north_of(good_fix(), 20))), None);
    assert_eq!(p.poll(base + 1_000, Some(north_of(good_fix(), 40))), None);
    assert_eq!(
        p.poll(base + 2_000, Some(north_of(good_fix(), 60))),
        Some(ReportReason::Movement)
    );
}

#[test]
fn a_station_never_reports_on_movement() {
    let mut p = ready(Profile::Station, 0);
    let far = north_of(good_fix(), 100_000);
    // Anywhere short of the heartbeat, however far it has been dragged.
    assert_eq!(p.poll(PolicyParams::STATION.settle_ms + 1, Some(far)), None);
    assert_eq!(
        p.poll(PolicyParams::STATION.max_interval_ms - 1, Some(far)),
        None
    );
    // And what does fire at the heartbeat is the heartbeat, not movement.
    assert_eq!(
        p.poll(PolicyParams::STATION.max_interval_ms, Some(far)),
        Some(ReportReason::Heartbeat)
    );
}

// ---------------------------------------------------------------------------
// Settle
// ---------------------------------------------------------------------------

#[test]
fn the_settle_window_suppresses_the_first_movement_report() {
    let mut p = SendPolicy::new();
    p.set_target(Profile::Tracker, true);
    // Immediate report at t=0 with no fix at all, so the settle anchor
    // is the first usable fix that follows.
    assert_eq!(p.poll(0, None), Some(ReportReason::Immediate));
    p.note_sent(0, None);

    let settle = PolicyParams::TRACKER.settle_ms;
    let min = PolicyParams::TRACKER.min_interval_ms;
    // First usable fix arrives at min (interval floor already clear).
    assert_eq!(p.poll(min, Some(good_fix())), None, "inside settle");
    assert_eq!(
        p.poll(min + settle - 1, Some(north_of(good_fix(), 500))),
        None,
        "still inside settle"
    );
    assert_eq!(
        p.poll(min + settle, Some(north_of(good_fix(), 500))),
        Some(ReportReason::Movement)
    );
}

#[test]
fn the_settle_window_does_not_hold_back_the_heartbeat() {
    // A cold start must still prove it is alive.
    let mut p = SendPolicy::new();
    p.set_target(Profile::Station, true);
    assert_eq!(p.poll(0, None), Some(ReportReason::Immediate));
    p.note_sent(0, None);
    let max = PolicyParams::STATION.max_interval_ms;
    // First usable fix lands one millisecond before the heartbeat, so
    // the settle window is wide open when it fires.
    assert_eq!(p.poll(max - 1, Some(good_fix())), None);
    assert_eq!(p.poll(max, Some(good_fix())), Some(ReportReason::Heartbeat));
}

// ---------------------------------------------------------------------------
// Accuracy threshold
// ---------------------------------------------------------------------------

#[test]
fn a_fix_worse_than_the_threshold_is_not_a_position() {
    let p = ready(Profile::Tracker, 0);
    let bad = Fix {
        hdop_e2: Some(PolicyParams::TRACKER.max_hdop_e2 + 1),
        ..good_fix()
    };
    assert!(!p.position_is_reportable(bad));
    let edge = Fix {
        hdop_e2: Some(PolicyParams::TRACKER.max_hdop_e2),
        ..good_fix()
    };
    assert!(p.position_is_reportable(edge));
}

#[test]
fn a_fix_without_an_accuracy_number_is_refused() {
    // "We do not know how good this is" is not "it is good".
    let p = ready(Profile::Tracker, 0);
    assert!(!p.position_is_reportable(Fix {
        hdop_e2: None,
        ..good_fix()
    }));
}

#[test]
fn an_inaccurate_fix_neither_reports_nor_moves_the_reference() {
    let mut p = ready(Profile::Tracker, 0);
    let base = PolicyParams::TRACKER.settle_ms + PolicyParams::TRACKER.min_interval_ms;
    let far_but_bad = Fix {
        hdop_e2: Some(900),
        ..north_of(good_fix(), 500)
    };
    assert_eq!(p.poll(base, Some(far_but_bad)), None);
    // The reference is still the reported position, so a good fix 20 m
    // from it is still under the gate.
    assert_eq!(p.poll(base + 1_000, Some(north_of(good_fix(), 20))), None);
}

#[test]
fn a_bad_fix_does_not_start_the_settle_window() {
    let mut p = SendPolicy::new();
    p.set_target(Profile::Tracker, true);
    assert_eq!(p.poll(0, None), Some(ReportReason::Immediate));
    p.note_sent(0, None);
    let settle = PolicyParams::TRACKER.settle_ms;
    let min = PolicyParams::TRACKER.min_interval_ms;
    // A stream of unusable fixes across the whole would-be settle window.
    for t in [min, min + settle / 2, min + settle] {
        assert_eq!(
            p.poll(
                t,
                Some(Fix {
                    hdop_e2: Some(900),
                    ..good_fix()
                })
            ),
            None
        );
    }
    // The first usable fix only now starts settling.
    let first_good = min + settle + 1;
    assert_eq!(p.poll(first_good, Some(good_fix())), None);
    assert_eq!(
        p.poll(first_good + settle - 1, Some(north_of(good_fix(), 500))),
        None
    );
    assert_eq!(
        p.poll(first_good + settle, Some(north_of(good_fix(), 500))),
        Some(ReportReason::Movement)
    );
}

// ---------------------------------------------------------------------------
// Distance arithmetic
// ---------------------------------------------------------------------------

#[test]
fn one_degree_of_latitude_is_one_degree_of_latitude() {
    let a = Fix {
        latitude_e6: 0,
        longitude_e6: 0,
        hdop_e2: Some(100),
    };
    let b = Fix {
        latitude_e6: 1_000_000,
        ..a
    };
    assert!(moved_at_least(a, b, 111_000));
    assert!(!moved_at_least(a, b, 112_000));
}

#[test]
fn longitude_shrinks_with_latitude() {
    // 0.001° of longitude is ~111 m at the equator and ~66 m at 53°N.
    let equator = Fix {
        latitude_e6: 0,
        longitude_e6: 0,
        hdop_e2: Some(100),
    };
    let equator_east = Fix {
        longitude_e6: 1_000,
        ..equator
    };
    assert!(moved_at_least(equator, equator_east, 110));
    assert!(!moved_at_least(equator, equator_east, 112));

    let north = Fix {
        latitude_e6: 53_000_000,
        longitude_e6: 0,
        hdop_e2: Some(100),
    };
    let north_east = Fix {
        longitude_e6: 1_000,
        ..north
    };
    assert!(moved_at_least(north, north_east, 66));
    assert!(!moved_at_least(north, north_east, 68));
}

#[test]
fn the_antimeridian_is_one_step_wide_not_three_hundred_and_fifty_nine_degrees() {
    let west = Fix {
        latitude_e6: 0,
        longitude_e6: -179_999_500,
        hdop_e2: Some(100),
    };
    let east = Fix {
        longitude_e6: 179_999_500,
        ..west
    };
    // 0.001° apart across the line: ~111 m, not ~40 000 km.
    assert!(moved_at_least(west, east, 110));
    assert!(!moved_at_least(west, east, 120));
}

#[test]
fn the_poles_do_not_overflow_or_divide_by_zero() {
    let np = Fix {
        latitude_e6: 90_000_000,
        longitude_e6: 0,
        hdop_e2: Some(100),
    };
    let sp = Fix {
        latitude_e6: -90_000_000,
        longitude_e6: 180_000_000,
        hdop_e2: Some(100),
    };
    // Pole to pole is ~20 000 km; the point is that it answers at all.
    assert!(moved_at_least(np, sp, 1_000_000));
    // Two points on the pole, a degree of longitude apart, are the same
    // place: cos(90°) is zero.
    let np_east = Fix {
        longitude_e6: 1_000_000,
        ..np
    };
    assert!(!moved_at_least(np, np_east, 1));
}

#[test]
fn the_cosine_table_tracks_the_real_cosine() {
    // Positive control for the table itself: a table filled with the
    // wrong constant would still pass every distance test that only
    // compares against itself.
    for deg in 0..=90u32 {
        let want = (deg as f64).to_radians().cos();
        let got = cos_lat_q15(deg as i32 * 1_000_000) as f64 / 32768.0;
        assert!(
            (want - got).abs() < 1e-4,
            "cos({deg}) table {got} vs real {want}"
        );
    }
    // And halfway between entries, where the interpolation lives.
    let want = 53.5f64.to_radians().cos();
    let got = cos_lat_q15(53_500_000) as f64 / 32768.0;
    assert!((want - got).abs() < 1e-4, "cos(53.5) {got} vs {want}");
}

// ---------------------------------------------------------------------------
// Defensive paths
// ---------------------------------------------------------------------------

#[test]
fn a_ready_target_with_nothing_armed_starts_its_clock_at_the_first_poll() {
    // Reachable only if a caller never confirms the immediate report and
    // then disarms it by hand; kept so the heartbeat cannot fire off a
    // zero timestamp inherited from boot.
    let mut p = SendPolicy::new();
    p.set_target(Profile::Station, true);
    p.note_sent(0, None);
    p.clear_target();
    p.set_target(Profile::Station, true);
    p.note_sent(1_000_000, None);
    assert_eq!(p.poll(1_000_001, None), None);
}

#[test]
fn expert_parameters_override_the_profile_preset() {
    let mut p = SendPolicy::new();
    p.set_target(Profile::Station, true);
    p.note_sent(0, None);
    p.set_params(PolicyParams {
        max_interval_ms: 5_000,
        ..PolicyParams::STATION
    });
    assert_eq!(p.poll(4_999, None), None);
    assert_eq!(p.poll(5_000, None), Some(ReportReason::Heartbeat));
}

// ---------------------------------------------------------------------------
// The control-frame chain: profile id in, target state out
// ---------------------------------------------------------------------------

#[test]
fn a_hash_only_frame_lands_in_awaiting_key_and_sends_nothing() {
    let mut p = SendPolicy::new();
    let command = command_from_wire(PROFILE_ID_STATION);
    assert_eq!(command, TargetCommand::Set(Profile::Station));
    assert_eq!(
        p.apply(command, false),
        TargetOutcome::Set(TargetState::AwaitingKey)
    );
    assert_eq!(p.state().as_str(), "awaiting-key");
    assert_eq!(p.poll(0, Some(good_fix())), None);
    assert_eq!(p.poll(24 * 60 * 60_000, Some(good_fix())), None);
}

#[test]
fn the_key_arriving_moves_it_to_ready_and_owes_one_report() {
    let mut p = SendPolicy::new();
    p.apply(command_from_wire(PROFILE_ID_STATION), false);
    assert!(p.note_key_available());
    assert_eq!(p.state().as_str(), "ready");
    assert_eq!(p.poll(1_000, None), Some(ReportReason::Immediate));
}

#[test]
fn a_clear_frame_switches_telemetry_off() {
    let mut p = SendPolicy::new();
    p.apply(command_from_wire(PROFILE_ID_TRACKER), true);
    p.note_sent(0, None);
    assert_eq!(command_from_wire(PROFILE_ID_OFF), TargetCommand::Clear);
    assert_eq!(
        p.apply(command_from_wire(PROFILE_ID_OFF), true),
        TargetOutcome::Cleared
    );
    assert_eq!(p.state(), TargetState::Off);
    assert_eq!(p.poll(24 * 60 * 60_000, Some(good_fix())), None);
}

#[test]
fn an_unknown_profile_id_runs_the_default_cadence_rather_than_losing_the_target() {
    let mut p = SendPolicy::new();
    assert_eq!(
        command_from_wire(0x7F),
        TargetCommand::Set(Profile::DEFAULT)
    );
    assert_eq!(
        p.apply(command_from_wire(0x7F), true),
        TargetOutcome::Set(TargetState::Ready)
    );
    assert_eq!(p.profile(), Profile::DEFAULT);
    assert_eq!(p.params(), Profile::DEFAULT.params());
}

#[test]
fn a_clear_frame_on_a_node_that_had_no_target_is_still_off() {
    let mut p = SendPolicy::new();
    assert_eq!(
        p.apply(command_from_wire(PROFILE_ID_OFF), false),
        TargetOutcome::Cleared
    );
    assert_eq!(p.state(), TargetState::Off);
}
