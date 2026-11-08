//! What one draw of the M9's chats list costs by how many conversations
//! the board holds (1581, #474): the 1489 fixture (`loquax/tests/support/
//! real_store.rs`) cut to its first `n` conversations, or copied under
//! other peers past its 300, pulled into a board's model page by page as
//! the board folds it (`loquax_m9::home::take`), then a key down the list
//! and the draw that answers it, split into its steps:
//!
//! - `due`: the loop's gate ([`screen::Drawn`]);
//! - `look`: [`Look::of`];
//! - `order`: the model's list order ([`UiModel::order`]);
//! - `scene`: [`screen::scene`], the order, the window and its rows;
//! - `draw`: [`Renderer::draw`], the cells laid out, the glyphs blitted;
//! - `page`: the `[PAGE]` line's place ([`page::of`]);
//! - `say`: what the frame showed, said to the model
//!   ([`loquax_m9::read::Reader::say`]), a model `update` or two;
//! - `tick`: the second's clock told the model (`UiEvent::Elapsed`), as
//!   the board's loop tells it once a second ([`Reader::tick`]);
//! - `rows`: the scene's window of rows built afresh (`scene::rows`, what
//!   a fold's note builds when the window's stamp moved: a key, a page);
//! - `names`: of that, the rows' names alone (`Names::label` and
//!   `Names::same_name`, each asking [`loquax::names::Names::collisions`]).
//!
//! ```sh
//! cargo run --release -p loquax-m9 --example list_measure -- [16 64 128 276 1000]
//! ```
//!
//! A row is the median of the runs after one warm-up, in microseconds on
//! the host; the board is about twenty times slower
//! (`docs/drafts/map-measurement-2026-10.md`). A measuring tool, not a gate:
//! `loquax-android/tests/name_lookups.rs` holds the names by count.

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../src/bin/loquax-m9-sim/real.rs"]
mod real;

use std::time::Instant;

use loquax::ui::history::Restored;
use loquax::ui::{update, UiEvent, UiModel};
use loquax_m9::field::Field;
use loquax_m9::frame::Frame;
use loquax_m9::group;
use loquax_m9::keys::{event, M9Key};
use loquax_m9::page;
use loquax_m9::read::Reader;
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look, Window};

/// The runs a row is the median of; `LIST_MEASURE_RUNS` for more, as a
/// profiler wants the turns to outweigh the pull.
fn runs() -> usize {
    std::env::var("LIST_MEASURE_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(21)
}
/// A board's page of pulled entries (window 16b: 234 a page).
const PAGE: usize = 234;

fn median(mut v: Vec<u128>) -> u128 {
    v.sort_unstable();
    v[v.len() / 2]
}

/// The fixture's first `n` conversations, past its 300 copied under
/// peers of their own (the copy's first byte says which copy).
fn fixture(n: usize) -> (Vec<Restored>, Vec<([u8; 16], String)>) {
    let base = real::real_store::restored();
    let names = real::real_store::names();
    let mut restored = Vec::new();
    let mut named = Vec::new();
    for i in 0..n {
        let copy = i / base.len();
        let mut r = base[i % base.len()].clone();
        let name = names
            .iter()
            .find(|(p, _)| *p == r.peer)
            .map_or_else(String::new, |(_, n)| n.clone());
        if copy > 0 {
            r.peer[1] ^= copy as u8;
            // A group copied is a conversation like any other here.
            r.group = None;
        }
        named.push((
            r.peer,
            if copy > 0 {
                format!("{name} c{copy}")
            } else {
                name
            },
        ));
        restored.push(r);
    }
    (restored, named)
}

fn main() -> Result<(), String> {
    let counts: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().map_err(|_| format!("not a count: {a}")))
        .collect::<Result<_, _>>()?;
    let counts = match counts.is_empty() {
        true => vec![16, 64, 128, 276, 1000],
        false => counts,
    };
    let own = real::address();
    println!(
        "LIST_DRAW n listed entries pull_ms key_us due_us look_us order_us scene_us draw_us page_us say_us tick_us total_us rows_us names_us"
    );
    for n in counts {
        let (restored, names) = fixture(n);
        let entries = real::archive_of(own, &restored, &names)?;
        let mut model: UiModel<Field> = loquax_m9::home::model(own);
        update(&mut model, UiEvent::Clock(real::real_store::now()));
        let t = Instant::now();
        for page in entries.chunks(PAGE) {
            loquax_m9::home::take(&mut model, page, &own);
        }
        let pull_ms = t.elapsed().as_millis();
        let listed = model.listed().count();

        let mut frame = Frame::new();
        let mut renderer = Renderer::new(&mut frame).map_err(|_| "renderer")?;
        let mut drawn = screen::Drawn::default();
        let mut row = 0;
        let mut spent: [Vec<u128>; 10] = Default::default();
        let mut reader = Reader::default();
        for run in 0..=runs() {
            // Down the list, then up again: the selection moves, the
            // window and the rows on screen with it.
            let key = if run % 2 == 0 { M9Key::Down } else { M9Key::Up };
            let t0 = Instant::now();
            group::press(&mut model, &mut row, &event(key));
            let t1 = Instant::now();
            let due = drawn.due(&model);
            let t2 = Instant::now();
            let look = Look {
                row,
                ..Look::of(&model)
            };
            let t3 = Instant::now();
            std::hint::black_box(model.order());
            let t4 = Instant::now();
            let scene = screen::scene(&model);
            let t5 = Instant::now();
            renderer.draw(&scene, &look).map_err(|_| "draw")?;
            let t6 = Instant::now();
            std::hint::black_box(page::of(&model, row, false, &Window::default()));
            let t7 = Instant::now();
            let uptime = 1_000 * (run as u64 + 1);
            std::hint::black_box(reader.say(&mut model, renderer.seen(), uptime));
            let t8 = Instant::now();
            std::hint::black_box(update(&mut model, UiEvent::Elapsed(uptime + 500)));
            let t9 = Instant::now();
            drawn.drawn(&model);
            std::hint::black_box(due);
            let _ = renderer.panel().take_dirty();
            if run == 0 {
                continue;
            }
            let steps = [t0, t1, t2, t3, t4, t5, t6, t7, t8, t9];
            for (k, pair) in steps.windows(2).enumerate() {
                spent[k].push(pair[1].duration_since(pair[0]).as_micros());
            }
            spent[9].push(t9.duration_since(t1).as_micros());
        }
        // The window as the fold builds it: twice the rows on screen.
        let order = model.order();
        let window: Vec<usize> = order
            .iter()
            .copied()
            .take(2 * model.viewing.list_rows.max(1))
            .collect();
        let mut rows_us = Vec::new();
        let mut names_us = Vec::new();
        for _ in 0..runs() {
            let t0 = Instant::now();
            std::hint::black_box(loquax::ui::scene::rows(&model, window.clone()));
            let t1 = Instant::now();
            for &i in &window {
                let peer = model.conversations[i].peer;
                std::hint::black_box(model.names.label(&peer, model.lang));
                std::hint::black_box(model.names.same_name(&peer));
            }
            rows_us.push(t1.duration_since(t0).as_micros());
            names_us.push(t1.elapsed().as_micros());
        }
        let m: Vec<u128> = spent.into_iter().map(median).collect();
        println!(
            "LIST_DRAW {n} {listed} {} {pull_ms} {} {} {} {} {} {} {} {} {} {} {} {}",
            entries.len(),
            m[0],
            m[1],
            m[2],
            m[3],
            m[4],
            m[5],
            m[6],
            m[7],
            m[8],
            m[9],
            median(rows_us),
            median(names_us)
        );
    }
    Ok(())
}
