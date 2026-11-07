//! The window's bars over the real fixture (1489, structure review
//! 2026-11-07 finding 14): counted with the core's operation counters,
//! never timed. Red at the commit that wrote them, judged through the
//! fixture's register (`loquax/tests/support/real_store.rs`, [`bar`]).

#[path = "../../tests/support/real_store.rs"]
mod real_store;

use super::tests::seen_of;
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

/// A pump of the window's drive with nothing changed builds nothing
/// (plan step C): the drive builds what the window is sent on every pass
/// (`Seen::of`, `gui.rs` `drive`), here through the tests' `seen_of`,
/// which builds the scene, the pins and the tracks the same way. Today a
/// pass builds the whole list and the whole open chat, fifty times a
/// second.
#[test]
fn a_pump_without_a_change_builds_nothing() {
    if !real_store::counting() {
        return;
    }
    let model = window_model();
    let _ = seen_of(&model);
    model.reset_counters();
    let _ = seen_of(&model);
    let counts = model.counters().expect("counting() said this build counts");
    println!("COUNTS window.idle_pump {counts}");
    bar(
        "window.a_pump_without_a_change_builds_nothing",
        counts.scene_nodes == 0 && counts.rows_built == 0 && counts.bubbles_built == 0,
        &format!(
            "scene_nodes {} rows_built {} bubbles_built {} (bar 0)",
            counts.scene_nodes, counts.rows_built, counts.bubbles_built
        ),
    );
}
