//! The messenger screens' heap for the M9 (1343), counted by a global
//! allocator on the host over the ugly store's model (20 conversations,
//! 800 messages): the restored model, the chats list scene, an open
//! chat's scene, a fresh group's scene and the member sheet, each built
//! at the M9's Compact class. A 64-bit host's figures: on the M9's
//! 32-bit ESP32-S3 the pointers and `usize`s halve, so they bound the
//! board's from above. The test prints the numbers and holds them to
//! ceilings a change that stops windowing the history, or copies the
//! store into the scene, breaks. The budget table they feed is
//! `docs/src/concepts/handheld-messenger.md`, "Memory budgets, measured".
//!
//! Since 1351 it counts the M9's renderer too (`loquax-m9`): the panel's
//! frame, mousefood and ratatui made once, the chats list's first draw and
//! a redraw with the cursor moved. Since 1356 the open chat as the M9
//! shows it: the scene the M9 builds with the keys on the list and a chat
//! open, the chat's first draw, a redraw with the cursor twelve bubbles
//! up, and one with a line typed into the compose field by the M9's keys. And the stack each step takes below
//! its caller, read off a painted stack: a debug build's frames on a
//! 64-bit host, so again bounds from above for the board's `opt-level =
//! "s"` build, which the board's own census replaces.
//!
//! One test in its own binary: the allocator counts the whole process,
//! so nothing may run beside it. It lives here, not in `loquax`, because
//! a counting allocator is `unsafe` code, which `loquax` forbids and
//! this crate allows where it is named.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};

use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use loquax::store::Store;
use loquax::ui::groups::Verb;
use loquax::ui::intent::Target;
use loquax::ui::scene::LayoutClass;
use loquax::ui::strings::Lang;
use loquax::ui::{update, Pane, UiEvent, UiModel, NONE_OPEN};
use loquax::when::WallTime;
use loquax_m9::field::Field;
use loquax_m9::frame::Frame;
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look};

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

struct Counting;

// The one unsafe here: a GlobalAlloc that counts and hands every call to
// the system's.

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live =
            LIVE.fetch_add(layout.size() as isize, Ordering::SeqCst) + layout.size() as isize;
        PEAK.fetch_max(live, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let delta = new as isize - layout.size() as isize;
        let live = LIVE.fetch_add(delta, Ordering::SeqCst) + delta;
        PEAK.fetch_max(live, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn live() -> isize {
    LIVE.load(Ordering::SeqCst)
}

/// Run `f`; return what it left on the heap and the peak above the start.
fn counted<R>(f: impl FnOnce() -> R) -> (R, isize, isize) {
    let before = live();
    PEAK.store(before, Ordering::SeqCst);
    let out = f();
    let held = live() - before;
    let peak = PEAK.load(Ordering::SeqCst) - before;
    (out, held, peak)
}

/// The stack a step may take: painted below the caller's frame before it
/// runs, read back after. A step deeper than this fails the census.
const PAINT_LEN: usize = 1 << 20;
const PAINT_BYTE: u8 = 0xA5;

/// Fill `PAINT_LEN` bytes of the stack below this frame with the paint;
/// where they begin.
#[inline(never)]
fn paint() -> usize {
    let mut area = [PAINT_BYTE; PAINT_LEN];
    std::hint::black_box(&mut area);
    area.as_ptr() as usize
}

/// How many bytes up from `low` still carry the paint.
#[inline(never)]
fn untouched_from(low: usize) -> usize {
    // The other unsafe here: reading back the stack below the live frames,
    // which this thread owns and nothing else writes.
    #[allow(unsafe_code)]
    let byte = |at: usize| unsafe { std::ptr::read_volatile(at as *const u8) };
    (0..PAINT_LEN)
        .position(|n| byte(low + n) != PAINT_BYTE)
        .unwrap_or(PAINT_LEN)
}

#[inline(never)]
fn run<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// Run `f`; return what it gave and the stack it took below this frame.
fn stack<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let low = paint();
    let out = run(f);
    let used = PAINT_LEN - untouched_from(low);
    assert!(used < PAINT_LEN, "the step took all {PAINT_LEN} B painted");
    (out, used)
}

/// The reader: no conversation's peer.
const OWN: usize = 0x30;

/// A day after the store's last row: every age and day line is fixed.
fn now() -> WallTime {
    let last = ugly_store::BASE
        + (ugly_store::PER_CONVERSATION as i64) * 600
        + (ugly_store::CONVERSATIONS as i64) * 7;
    WallTime {
        unix_secs: last + 86_400,
        offset_secs: 3600,
    }
}

/// The model a terminal loquax restores from the ugly store, as the shell
/// builds it: contacts, names, the history, the positions.
fn ugly_model(home: &Path) -> UiModel<Field> {
    let own = ugly_store::identity(OWN);
    ugly_store::fill_store_of(home, &own);
    let me = loquax::engine::delivery_address(&own).expect("address");
    let store = Store::open_in(home, me).expect("the store");
    let contacts = loquax::names::load_contacts(home).expect("contacts");
    let mut model = UiModel::<Field>::new(&[]);
    model.address = Some(me);
    model.names = loquax::names::Names::new(contacts);
    for (peer, name) in store.names().expect("names") {
        model.names.remember(peer, &name);
    }
    update(&mut model, UiEvent::Clock(now()));
    model.restore(store.restore().expect("restore"));
    model.restore_positions(store.positions().expect("positions"));
    model
}

#[test]
fn the_m9_screens_heap_over_the_ugly_store() {
    // A stack the painted megabyte fits under, whatever the harness gives.
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(census)
        .expect("the census thread")
        .join()
        .expect("the census");
}

fn census() {
    // The stack count's positive control: a step that holds 64 KiB on its
    // frame is counted at least that deep, and not much deeper.
    let (_, known) = stack(|| {
        let area = [1u8; 64 << 10];
        std::hint::black_box(&area);
    });
    assert!(
        ((64 << 10)..(64 << 10) + 16_384).contains(&known),
        "a 64 KiB frame counted as {known} B of stack"
    );

    let dir = tempfile::tempdir().expect("tempdir");

    let (mut model, model_held, _) = counted(|| ugly_model(dir.path()));
    model.selected = NONE_OPEN;
    println!("model restored: held {model_held} B");

    let ((scene, list_held, list_peak), list_stack) =
        stack(|| counted(|| model.scene(LayoutClass::Compact)));
    println!("chats list scene: held {list_held} B, peak {list_peak} B, stack {list_stack} B");
    assert!(
        !scene.list.as_ref().expect("the list").rows.is_empty(),
        "the list scene carries rows (the measurements' positive control)"
    );

    // The M9's renderer over that scene: the frame, the renderer, the
    // first draw, then a redraw with the cursor on the fourteenth row,
    // past the first screen.
    let look = Look::m9(Lang::En, Some(now()));
    let (mut frame, frame_held, _) = counted(Frame::new);
    println!("panel frame: held {frame_held} B");
    let ((renderer, renderer_held, renderer_peak), renderer_stack) =
        stack(|| counted(|| Renderer::new(&mut frame).expect("renderer")));
    let mut renderer = renderer;
    println!(
        "renderer made: held {renderer_held} B, peak {renderer_peak} B, \
         stack {renderer_stack} B"
    );
    let ((drawn, draw_held, draw_peak), draw_stack) =
        stack(|| counted(|| renderer.draw(&scene, &look)));
    drawn.expect("the first draw");
    println!("chats list first draw: held {draw_held} B, peak {draw_peak} B, stack {draw_stack} B");
    let mut moved = scene.clone();
    for (i, row) in moved
        .list
        .as_mut()
        .expect("the list")
        .rows
        .iter_mut()
        .enumerate()
    {
        row.item.selected = i == 13;
    }
    let ((drawn, redraw_held, redraw_peak), redraw_stack) =
        stack(|| counted(|| renderer.draw(&moved, &look)));
    drawn.expect("the redraw");
    println!(
        "chats list redraw, cursor moved: held {redraw_held} B, peak {redraw_peak} B, \
         stack {redraw_stack} B"
    );
    drop(renderer);
    assert!(
        frame.pixels().iter().any(|&p| p != 0),
        "the renderer drew into the frame (the measurements' positive control)"
    );
    drop(frame);
    drop(scene);
    drop(moved);

    model.selected = ugly_store::PINNED;
    let ((scene, chat_held, chat_peak), chat_stack) =
        stack(|| counted(|| model.scene(LayoutClass::Compact)));
    println!("open chat scene: held {chat_held} B, peak {chat_peak} B, stack {chat_stack} B");
    assert!(
        !scene.chat.as_ref().expect("the chat").groups.is_empty(),
        "the chat scene carries bubbles (the measurements' positive control)"
    );
    drop(scene);

    // Slice two: the open chat as the M9 shows it. With the keys on the
    // list the M9 builds the Wide scene and drops its chat (the list's
    // peak carries the chat's build); with them in the chat, the Compact
    // one. Then the chat's first draw, a redraw with the cursor twelve
    // bubbles up, and one with a line typed in by the M9's keys.
    let key = |model: &mut UiModel<Field>, code: KeyCode| {
        update(model, UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    };
    model.focus = Pane::Conversations;
    let (scene, listed_held, listed_peak) = counted(|| screen::scene(&model));
    println!("list with a chat open (M9): held {listed_held} B, peak {listed_peak} B");
    assert!(scene.list.is_some() && scene.chat.is_none());
    drop(scene);
    key(&mut model, KeyCode::Enter);
    assert_eq!(model.focus, Pane::Messages);
    let mut frame = Frame::new();
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    let ((scene, m9_chat_held, m9_chat_peak), m9_chat_stack) =
        stack(|| counted(|| screen::scene(&model)));
    println!(
        "open chat scene (M9): held {m9_chat_held} B, peak {m9_chat_peak} B, \
         stack {m9_chat_stack} B"
    );
    let look = Look::of(&model);
    let ((drawn, chat_draw_held, chat_draw_peak), chat_draw_stack) =
        stack(|| counted(|| renderer.draw(&scene, &look)));
    drawn.expect("the chat's first draw");
    // The renderer keeps the keys of the bubbles the frame showed (1541,
    // `Renderer::seen`, what the board says the model after the draw):
    // the first chat draw makes that list, the list's draws before kept
    // none. Its bytes are the renderer's own state, not the draw's; the
    // census held a chat draw to 0 B before 1541 and went red at 64 B:
    // six keys of eight bytes in a list grown by push to room for eight
    // (1581). Up to twice the keys' bytes are the list's; more is a leak.
    let seen_bytes = renderer.seen().map_or(0, core::mem::size_of_val) as isize;
    println!(
        "open chat first draw: held {chat_draw_held} B, {seen_bytes} B of them the keys it \
         showed, peak {chat_draw_peak} B, stack {chat_draw_stack} B"
    );
    let chat_draw_held = match chat_draw_held <= 2 * seen_bytes {
        true => 0,
        false => chat_draw_held,
    };
    drop(scene);
    for _ in 0..12 {
        key(&mut model, KeyCode::Up);
    }
    let scene = screen::scene(&model);
    let look = Look::of(&model);
    let ((drawn, cursor_held, cursor_peak), cursor_stack) =
        stack(|| counted(|| renderer.draw(&scene, &look)));
    drawn.expect("the chat's redraw");
    println!(
        "open chat redraw, cursor moved: held {cursor_held} B, peak {cursor_peak} B, \
         stack {cursor_stack} B"
    );
    drop(scene);
    key(&mut model, KeyCode::Char('i'));
    for c in "Bis gleich in Köln!".chars() {
        key(&mut model, KeyCode::Char(c));
    }
    assert_eq!(model.focus, Pane::Compose);
    let scene = screen::scene(&model);
    let look = Look::of(&model);
    let ((drawn, typed_held, typed_peak), typed_stack) =
        stack(|| counted(|| renderer.draw(&scene, &look)));
    drawn.expect("the compose line's redraw");
    println!(
        "open chat redraw, a line typed: held {typed_held} B, peak {typed_peak} B, \
         stack {typed_stack} B"
    );
    drop(scene);
    drop(renderer);
    assert!(
        frame.pixels().iter().any(|&p| p != 0),
        "the chat was drawn into the frame (the measurements' positive control)"
    );
    drop(frame);

    // A fresh group with three members, as the picker makes it, then its
    // member sheet.
    model.selected = NONE_OPEN;
    update(&mut model, UiEvent::Group(Verb::New));
    for i in [2usize, 3, 5] {
        update(&mut model, UiEvent::Group(Verb::Pick(ugly_store::peer(i))));
    }
    update(&mut model, UiEvent::Group(Verb::Next));
    update(&mut model, UiEvent::Group(Verb::Name("Familie".into())));
    update(&mut model, UiEvent::Group(Verb::Create));
    update(&mut model, UiEvent::GroupMade(Ok([0xAA; 16])));
    let (scene, group_held, group_peak) = counted(|| model.scene(LayoutClass::Compact));
    println!("fresh group scene: held {group_held} B, peak {group_peak} B");
    drop(scene);
    update(
        &mut model,
        UiEvent::Group(Verb::Members {
            group: Target::Open,
        }),
    );
    let (scene, members_held, members_peak) = counted(|| model.scene(LayoutClass::Compact));
    println!("member sheet scene: held {members_held} B, peak {members_peak} B");
    assert!(
        scene.members.is_some(),
        "the member sheet is in the scene (the measurements' positive control)"
    );
    drop(scene);

    // The ceilings: about double the 2026-11-03 figures (447,639 /
    // 26,532 / 111,184 held, 145,638 peak), so growth is caught before
    // it owns the board's RAM, not on every added field.
    assert!(
        model_held < 900_000,
        "the restored model held {model_held} B: the ugly store's model \
         fits twice over in the figure of 1343"
    );
    assert!(
        list_held < 60_000 && list_peak < 70_000,
        "the chats list scene held {list_held} B, peak {list_peak} B: \
         a list scene is rows, not the history"
    );
    assert!(
        chat_held < 250_000 && chat_peak < 320_000,
        "the open chat scene held {chat_held} B, peak {chat_peak} B: \
         the scene windows the history, it does not copy the store"
    );
    // The renderer's: about double the 2026-11-03 figures (107,584 made,
    // 86,840 peak a draw, none held; stack 49,672 and 67,384 to build the
    // two scenes, 15,744 to make the renderer, 6,656 to draw), so a draw
    // that keeps what it laid out, or a renderer that grows a second
    // frame, is caught.
    assert!(
        renderer_held < 220_000 && draw_peak < 180_000 && redraw_peak < 180_000,
        "the renderer held {renderer_held} B, a draw peaked at {draw_peak} B \
         and {redraw_peak} B: mousefood draws into the caller's frame"
    );
    assert!(
        draw_held == 0 && redraw_held == 0,
        "a draw held {draw_held} B and a redraw {redraw_held} B: a draw keeps \
         nothing past the cells ratatui already holds"
    );
    assert!(
        list_stack < 100_000 && chat_stack < 140_000,
        "building the list took {list_stack} B of stack, the chat {chat_stack} B"
    );
    assert!(
        renderer_stack < 32_000 && draw_stack < 16_000 && redraw_stack < 16_000,
        "making the renderer took {renderer_stack} B of stack, a draw \
         {draw_stack} B and {redraw_stack} B"
    );
    // The open chat on the M9 (1356), about double the 2026-11-03 figures:
    // the scene as the Compact one above (111,184 held, 145,638 peak,
    // 81,560 stack), the list with a chat open the same peak (the list and
    // the chat built, the chat dropped); a draw (81,056 B peak, 11,312 B
    // stack) lays out only the bubbles on screen and keeps nothing.
    assert!(
        m9_chat_held < 250_000 && m9_chat_peak < 320_000 && listed_peak < 320_000,
        "the M9's open chat scene held {m9_chat_held} B, peak {m9_chat_peak} B; \
         the list with a chat open peaked at {listed_peak} B"
    );
    assert!(
        chat_draw_peak < 180_000 && cursor_peak < 180_000 && typed_peak < 180_000,
        "a chat draw peaked at {chat_draw_peak} B, {cursor_peak} B and \
         {typed_peak} B: only the bubbles on screen are laid out"
    );
    assert!(
        chat_draw_held == 0 && cursor_held == 0 && typed_held == 0,
        "a chat draw held {chat_draw_held} B, {cursor_held} B, {typed_held} B"
    );
    assert!(
        m9_chat_stack < 160_000
            && chat_draw_stack < 24_000
            && cursor_stack < 24_000
            && typed_stack < 24_000,
        "the chat's scene took {m9_chat_stack} B of stack, its draws \
         {chat_draw_stack} B, {cursor_stack} B and {typed_stack} B"
    );
    assert!(
        group_peak < 80_000 && members_held < 20_000 && members_peak < 80_000,
        "group {group_peak} B peak, members held {members_held} B, \
         peak {members_peak} B: a sheet is rows over the open scene"
    );
}
