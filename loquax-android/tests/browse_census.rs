//! The Browse tab's heap (1337), counted by a global allocator on the
//! host: what the hosted browser holds in a Loquax model before the tab is
//! opened, on its start list, and with the largest page we have (the
//! Reticulum README) laid out on the M9's 53 by 24 cells, and the peak of
//! one scene build over it. A 64-bit host's figures: on the M9's 32-bit
//! ESP32-S3 the pointers and `usize`s halve, so they bound the board's
//! from above. The test prints them and holds them to ceilings a change
//! that copies the page, or builds the whole page into the scene, breaks.
//!
//! One test in its own binary: the allocator counts the whole process, so
//! nothing may run beside it. It lives here, not in `loquax`, because a
//! counting allocator is `unsafe` code, which `loquax` forbids and this
//! crate allows where it is named.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use leviculum_ui::input::{InputBuffer, KeyEvent};
use lnomad::DiscoveredNode;
use loquax::ui::browse::BrowseEvent;
use loquax::ui::scene::{LayoutClass, TabId};
use loquax::ui::{update, UiEvent, UiModel};

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
    fn newline(&mut self) {}
    fn set_cursor(&mut self, _at: usize) {}
    fn set_value(&mut self, text: &str) {
        self.0 = text.to_string();
    }
}

#[test]
fn the_browse_tab_heap_on_the_m9_cells() {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lnomad/tests_data/reticulum_readme.mu"),
    )
    .expect("the README page");

    // The browser a Loquax model carries before the tab ever opens.
    let base = live();
    let browse = loquax::ui::browse::Browse::default();
    let idle = live() - base;
    drop(browse);

    let mut model = UiModel::<Line>::new(&[]);
    model.browse.enabled = true;
    let before = live();
    for n in 0..20u8 {
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Node(DiscoveredNode {
                dest_hash: [n; 16],
                name: Some(format!("node {n}")),
                first_seen: 1_800_000_000,
                last_seen: 1_800_000_000,
                hops: Some(2),
            })),
        );
    }
    update(&mut model, UiEvent::Tab(TabId::Browse));
    let list = live() - before;

    let before = live();
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Loaded {
            micron: readme.clone(),
            title: "Reticulum".to_string(),
        }),
    );
    let page = live() - before;

    let before = live();
    PEAK.store(before, Ordering::SeqCst);
    let scene = model.scene(LayoutClass::Compact);
    let scene_held = live() - before;
    let scene_peak = PEAK.load(Ordering::SeqCst) - before;
    let runs = scene.browse.as_ref().map_or(0, |b| b.page.runs());
    drop(scene);

    println!(
        "census: idle browser {idle} B; tab open on 20 nodes +{list} B; \
         README ({} B of micron) loaded at 53x24 +{page} B; one scene \
         holds {scene_held} B, peak {scene_peak} B, {runs} runs",
        readme.len()
    );
    // The page is the model's cell layout, as the concept page measured
    // (951 KB for the README at 53 cells, 48 B a cell on the host); the
    // scene a window of it, never the page.
    assert!(page < 1_400_000, "the README's layout grew: {page} B");
    assert!(
        scene_held < 64 * 1024,
        "the scene is a window: {scene_held} B"
    );
    assert!(runs <= 67, "a README window holds at most 67 runs: {runs}");
    assert!(idle < 8 * 1024, "an unopened tab costs little: {idle} B");
}
