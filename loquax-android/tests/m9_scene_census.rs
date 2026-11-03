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
//! One test in its own binary: the allocator counts the whole process,
//! so nothing may run beside it. It lives here, not in `loquax`, because
//! a counting allocator is `unsafe` code, which `loquax` forbids and
//! this crate allows where it is named.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};

use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::store::Store;
use loquax::ui::groups::Verb;
use loquax::ui::intent::Target;
use loquax::ui::scene::LayoutClass;
use loquax::ui::{update, UiEvent, UiModel, NONE_OPEN};
use loquax::when::WallTime;

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

/// A compose field nobody types into.
#[derive(Debug, Default, Clone)]
struct Line(String);

impl InputBuffer for Line {
    fn value(&self) -> &str {
        &self.0
    }
    fn cursor(&self) -> usize {
        self.0.chars().count()
    }
    fn edit(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
    fn newline(&mut self) {
        self.0.push('\n');
    }
    fn set_cursor(&mut self, _at: usize) {}
    fn set_value(&mut self, text: &str) {
        self.0 = text.to_string();
    }
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
fn ugly_model(home: &Path) -> UiModel<Line> {
    let own = ugly_store::identity(OWN);
    ugly_store::fill_store_of(home, &own);
    let me = loquax::engine::delivery_address(&own).expect("address");
    let store = Store::open_in(home, me).expect("the store");
    let contacts = loquax::names::load_contacts(home).expect("contacts");
    let mut model = UiModel::<Line>::new(&[]);
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
    let dir = tempfile::tempdir().expect("tempdir");

    let (mut model, model_held, _) = counted(|| ugly_model(dir.path()));
    model.selected = NONE_OPEN;
    println!("model restored: held {model_held} B");

    let (scene, list_held, list_peak) = counted(|| model.scene(LayoutClass::Compact));
    println!("chats list scene: held {list_held} B, peak {list_peak} B");
    assert!(
        !scene.list.as_ref().expect("the list").rows.is_empty(),
        "the list scene carries rows (the measurements' positive control)"
    );
    drop(scene);

    model.selected = ugly_store::PINNED;
    let (scene, chat_held, chat_peak) = counted(|| model.scene(LayoutClass::Compact));
    println!("open chat scene: held {chat_held} B, peak {chat_peak} B");
    assert!(
        !scene.chat.as_ref().expect("the chat").groups.is_empty(),
        "the chat scene carries bubbles (the measurements' positive control)"
    );
    drop(scene);

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
    assert!(
        group_peak < 80_000 && members_held < 20_000 && members_peak < 80_000,
        "group {group_peak} B peak, members held {members_held} B, \
         peak {members_peak} B: a sheet is rows over the open scene"
    );
}
