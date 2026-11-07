//! The M9's bar over the real fixture (1489, structure review 2026-11-07
//! finding 14): a key in the chats list builds the rows the screen shows,
//! counted with the core's operation counters, never timed. Red at the
//! commit that wrote it (the list built whole, 1463's 3.2 s a key on the
//! board), judged through the fixture's register
//! (`loquax/tests/support/real_store.rs`).

#[path = "../../loquax/tests/support/real_store.rs"]
mod real_store;

use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use loquax::ui::{update, Pane, UiEvent, UiModel};
use loquax_m9::field::Field;
use loquax_m9::screen::{self, ENTRY_ROWS, ROWS};
use real_store::bar;

/// A key down the chats list on the M9 builds at most the list's entries
/// on its screen, and as many again: what [`screen::scene`] asks of the
/// core for the list.
#[test]
fn a_key_in_the_list_builds_the_rows_on_screen() {
    if !real_store::counting() {
        return;
    }
    real_store::check_shape();
    let mut model: UiModel<Field> = real_store::model();
    model.focus = Pane::Conversations;
    // As the board's model is built (loquax-m9/src/home.rs): the scene
    // is built for the M9's screen, not the store.
    update(&mut model, UiEvent::Viewing(screen::viewing()));
    let _ = screen::scene(&model);
    update(
        &mut model,
        UiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
    );
    let _ = screen::scene(&model);
    let counts = model.counters().expect("counting() said this build counts");
    println!("COUNTS m9.list_key {counts}");
    let most = 2 * (ROWS / ENTRY_ROWS) as u32;
    bar(
        "m9.a_key_in_the_list_builds_the_rows_on_screen",
        counts.rows_built <= most && counts.bubbles_built == 0,
        &format!(
            "rows_built {} (bar {most}), bubbles_built {} (bar 0)",
            counts.rows_built, counts.bubbles_built
        ),
    );
}
