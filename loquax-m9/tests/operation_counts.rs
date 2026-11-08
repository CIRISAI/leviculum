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
use loquax_m9::keyboard::POLL_MS;
use loquax_m9::screen::{self, ENTRY_ROWS};
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
    update(
        &mut model,
        UiEvent::Viewing(screen::viewing(screen::Size::DEFAULT)),
    );
    let _ = screen::scene(&model);
    update(
        &mut model,
        UiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
    );
    let _ = screen::scene(&model);
    let counts = model.counters().expect("counting() said this build counts");
    println!("COUNTS m9.list_key {counts}");
    // The screen's entries in the size the board shows (1532).
    let most = 2 * (screen::Size::DEFAULT.grid().1 / ENTRY_ROWS) as u32;
    bar(
        "m9.a_key_in_the_list_builds_the_rows_on_screen",
        counts.rows_built <= most && counts.bubbles_built == 0,
        &format!(
            "rows_built {} (bar {most}), bubbles_built {} (bar 0)",
            counts.rows_built, counts.bubbles_built
        ),
    );
}

/// Five seconds of the board's loop with nothing pulled draw nothing
/// (1491): every turn (`POLL_MS` apart on the board,
/// `leviculum-esp/src/bin/thinknode_m9.rs`) folds what the client
/// drained, an empty page, and asks the loop's gate ([`screen::Drawn`])
/// before it draws. The control: a page with a new message draws once.
#[test]
fn an_idle_board_draws_nothing() {
    if !real_store::counting() {
        return;
    }
    let mut model: UiModel<Field> = real_store::model();
    update(
        &mut model,
        UiEvent::Viewing(screen::viewing(screen::Size::DEFAULT)),
    );
    let address = real_store::own();
    let mut drawn = screen::Drawn::default();
    let _ = screen::scene(&model);
    drawn.drawn(&model);
    let mut scenes = 0;
    let turns = real_store::IDLE_MS / u64::from(POLL_MS);
    for _ in 0..turns {
        loquax_m9::home::take(&mut model, &[], &address);
        if drawn.due(&model) {
            scenes += 1;
            let _ = screen::scene(&model);
        }
        drawn.drawn(&model);
    }
    // The control: a friend's message, as a drained page folds it.
    update(
        &mut model,
        UiEvent::Engine(loquax::outbox::OutboxEvent::Received {
            sent_at: real_store::BASE + 900,
            message_id: [0x91; 32],
            source: real_store::peer(12),
            title: Vec::new(),
            body: b"Bist du da?".to_vec(),
            fields: Vec::new(),
            verified: true,
        }),
    );
    let control = u32::from(drawn.due(&model));
    println!("COUNTS m9.idle turns={turns} scenes={scenes} control={control}");
    assert_eq!(control, 1, "the control drew nothing");
    bar(
        "m9.an_idle_board_draws_nothing",
        scenes == 0 && drawn.builds() == 1,
        &format!(
            "scenes {scenes} over {turns} turns, {} draws allowed (bar 0)",
            drawn.builds() - 1
        ),
    );
}

/// The host's microseconds since the first call.
fn host_us() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

/// 1591: where the list's draw goes on the host, split as the board's
/// `[LOOP] core=ui` line splits it (`render::DrawSplit`): the scene's
/// rows, their layout into cells, the changed cells into the frame.
/// Forty keys down the 1489 fixture's list, and forty arrivals that move
/// a row to the top under the cursor (a pull's page). Printed, not
/// barred: the board's own line is the bar (window 19); the host says
/// which part leads.
#[test]
fn the_list_draw_split() {
    use loquax_m9::frame::Frame;
    use loquax_m9::map::{Map, FIXTURE};
    use loquax_m9::render::{DrawSplit, Renderer};
    use loquax_m9::screen::Look;
    let mut model: UiModel<Field> = real_store::model();
    model.focus = Pane::Conversations;
    model.list_apart = true;
    update(
        &mut model,
        UiEvent::Viewing(screen::viewing(screen::Size::DEFAULT)),
    );
    let mut frame = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    renderer.set_clock(host_us);
    let mut draw = |model: &UiModel<Field>, renderer: &mut Renderer<'_, Frame>| -> DrawSplit {
        renderer
            .show(model, &Look::of(model), &mut map)
            .expect("draw");
        renderer.take_split()
    };
    let _ = draw(&model, &mut renderer);
    let mut keys = Vec::new();
    for _ in 0..40 {
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
        keys.push(draw(&model, &mut renderer));
    }
    let mut arrivals = Vec::new();
    for n in 0..40u8 {
        update(
            &mut model,
            UiEvent::Engine(loquax::outbox::OutboxEvent::Received {
                sent_at: real_store::BASE + 1000 + i64::from(n),
                message_id: [0xa0u8.wrapping_add(n); 32],
                source: real_store::peer(20 + usize::from(n)),
                title: Vec::new(),
                body: format!("Nachricht {n}").into_bytes(),
                fields: Vec::new(),
                verified: true,
            }),
        );
        arrivals.push(draw(&model, &mut renderer));
    }
    let say = |what: &str, splits: &[DrawSplit]| {
        let part = |f: fn(&DrawSplit) -> u64| {
            let mut v: Vec<u64> = splits.iter().map(f).collect();
            v.sort_unstable();
            (v[v.len() / 2], v[v.len() - 1])
        };
        let (rows, layout, blit) = (
            part(|s| s.scene_us),
            part(|s| s.layout_us),
            part(|s| s.blit_us),
        );
        println!(
            "DRAW_SPLIT {what} n={} rows_us={}/{} layout_us={}/{} blit_us={}/{} (median/max)",
            splits.len(),
            rows.0,
            rows.1,
            layout.0,
            layout.1,
            blit.0,
            blit.1
        );
    };
    say("keys", &keys);
    say("arrivals", &arrivals);
}
