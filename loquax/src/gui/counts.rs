//! The window's bars over the real fixture (1489, structure review
//! 2026-11-07 finding 14): counted with the core's operation counters,
//! never timed. Red at the commit that wrote them, judged through the
//! fixture's register (`loquax/tests/support/real_store.rs`, [`bar`]).

#[path = "../../tests/support/real_store.rs"]
mod real_store;

use super::*;
use loquax::ui::update;
use real_store::bar;

/// The fixture's model in the window, the long chat open.
fn window_model() -> crate::shell::Model {
    let mut model: crate::shell::Model = real_store::model();
    update(
        &mut model,
        UiEvent::Open {
            peer: real_store::peer(real_store::ANNA),
        },
    );
    model
}

/// Five seconds of the window's drive left alone build nothing (plan
/// step C, 1491): the clock's events and the engine's pump fold as the
/// shell folds them, and after each the drive's pass ([`Driving`], as
/// `drive` in `gui.rs` runs it) builds what the window is sent
/// ([`Seen::of_model`], the drive's own builder, not a copy) only when
/// its stamp moved. Before 1491 every pass built the whole list and the
/// whole open chat, fifty times a second. The control builds on every
/// pass, as the drive did.
#[test]
fn a_pump_without_a_change_builds_nothing() {
    if !real_store::counting() {
        return;
    }
    let mut model = window_model();
    let (builds, counts) = idle_drive(&mut model, true);
    println!("COUNTS window.idle_pump builds={builds} {counts}");
    let (every, control) = idle_drive(&mut model, false);
    println!("COUNTS window.idle_pump_control builds={every} {control}");
    assert!(
        every >= 200 && control.scene_nodes > 0,
        "the control built nothing: {every} builds, {control}"
    );
    bar(
        "window.a_pump_without_a_change_builds_nothing",
        builds == 0 && counts.scene_nodes == 0 && counts.rows_built == 0,
        &format!(
            "builds {builds} scene_nodes {} rows_built {} bubbles_built {} over {} ms (bar 0)",
            counts.scene_nodes,
            counts.rows_built,
            counts.bubbles_built,
            real_store::IDLE_MS
        ),
    );
}

/// The states the drive built over [`real_store::idle`] once it had sent
/// its first, gated on the stamp (`gated`) or on every pass, and what the
/// core built for them.
fn idle_drive(
    model: &mut crate::shell::Model,
    gated: bool,
) -> (u64, loquax::ui::counters::Counters) {
    let browsing = crate::browsing::Browsing::off();
    let mut driving = Driving::default();
    let stamp = |model: &crate::shell::Model| match gated {
        true => Seen::stamp(model, 0, &browsing, None),
        false => u64::from(driving_turn()),
    };
    let _ = driving.pass(stamp(model), || Seen::of_model(model, 0, &browsing));
    let first = driving.builds();
    let mut total = loquax::ui::counters::Counters::default();
    real_store::idle(model, 0, |model| {
        let _ = driving.pass(stamp(model), || Seen::of_model(model, 0, &browsing));
        total = total + model.counters().expect("counting() said this build counts");
    });
    (driving.builds() - first, total)
}

/// A stamp that moves on every call, never one seen before: the
/// control's, a drive that builds on every pass.
fn driving_turn() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static TURN: AtomicU32 = AtomicU32::new(0);
    TURN.fetch_add(1, Ordering::Relaxed)
}
