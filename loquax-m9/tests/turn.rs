//! 1463: what one turn of the board's loop costs does not grow with the
//! message store. Measured on the board 2026-11-07 (4067 entries): a key
//! on the chats list took 3.2 s to its flush, a restore 47 s, and the pull
//! slowed page by page, because the list walked every message on every
//! draw (`Conversation::last_written` under the sort, the unread count,
//! the avatar's appearance, and the open chat's 4000 bubbles built and
//! thrown away). The host is a thousand times faster than the board, so
//! the test does not hold a turn to milliseconds: it holds the turn at
//! 4000 entries to the turn at 400, where a walk over every message costs
//! ten times as much.
//!
//! Each measurement is the least of several, so a busy host only makes
//! both sides slower. The positive control is the same measurement over
//! the scene the M9 drew before (the full one, its chat built): it grows.

use std::time::{Duration, Instant};

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::scene::LayoutClass;
use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::home;
use loquax_m9::screen::{self, Look, COLS, ROWS};
use rand_core::OsRng;

/// The page the board pulls: 72 entries, as the bench's first page.
const PAGE: usize = 72;
/// A walk over every message costs ten times as much at the larger size;
/// what does not walk them stays within this.
const BOUND: u32 = 3;

struct Archive {
    me: Identity,
    address: [u8; 16],
    friends: Vec<Identity>,
}

impl Archive {
    fn new() -> Self {
        let me = Identity::generate(&mut OsRng);
        let address = lxmf_address(&me.public_key_bytes()).expect("address");
        let friends = (0..12).map(|_| Identity::generate(&mut OsRng)).collect();
        Self {
            me,
            address,
            friends,
        }
    }

    /// Entries `from..to`, the shape of Lew's archive: twelve friends, one
    /// in five ours.
    fn entries(&self, from: u64, to: u64) -> Vec<Entry> {
        (from..to)
            .map(|seq| {
                let friend = &self.friends[seq as usize % self.friends.len()];
                let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
                let at = 1_762_000_000.0 + seq as f64;
                let (kind, from, to, body) = match seq % 5 {
                    0 => (KIND_SENT, &self.me, theirs, format!("Antwort {seq}")),
                    _ => (
                        KIND_RECEIVED,
                        friend,
                        self.address,
                        format!("Nachricht {seq}"),
                    ),
                };
                let source = lxmf_address(&from.public_key_bytes()).expect("address");
                let packed = Message::create(
                    to,
                    source,
                    from,
                    at,
                    Vec::new(),
                    body.into_bytes(),
                    Vec::new(),
                    DeliveryMethod::Direct,
                )
                .expect("create")
                .pack();
                Entry {
                    seq,
                    kind,
                    at: at as i64,
                    packed,
                }
            })
            .collect()
    }

    /// The board's model after `n` entries came in pages.
    fn model(&self, all: &[Entry], n: usize) -> UiModel<Field> {
        let mut model = home::model(self.address);
        for page in all[..n].chunks(PAGE) {
            home::take(&mut model, page, &self.address);
        }
        model
    }
}

/// The least of nine runs of `f`.
fn least(mut f: impl FnMut()) -> Duration {
    (0..9)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .expect("nine runs")
}

/// What the M9 draws of the list: its scene and its cells.
fn list_turn(model: &UiModel<Field>) {
    let scene = screen::scene(model);
    std::hint::black_box(screen::lines(&scene, &Look::of(model), COLS, ROWS));
}

/// The draw before 1463: the full scene, its chat built and dropped.
fn full_turn(model: &UiModel<Field>) {
    std::hint::black_box(model.scene(LayoutClass::Wide));
}

#[test]
fn a_turn_costs_the_same_at_4000_entries_as_at_400() {
    let archive = Archive::new();
    let all = archive.entries(1, 4001 + 9 * PAGE as u64);
    let small = archive.model(&all, 400);
    let large = archive.model(&all, 4000);

    // The list's draw.
    let at_400 = least(|| list_turn(&small));
    let at_4000 = least(|| list_turn(&large));
    // The positive control: the full scene walks every message.
    let full_400 = least(|| full_turn(&small));
    let full_4000 = least(|| full_turn(&large));
    // A page taken into a model of 400 and of 4000 entries: nine fresh
    // pages each, the least of them.
    let pages = |mut model: UiModel<Field>, from: usize| {
        (0..9)
            .map(|i| {
                let page = &all[from + i * PAGE..from + (i + 1) * PAGE];
                let t = Instant::now();
                home::take(&mut model, page, &archive.address);
                t.elapsed()
            })
            .min()
            .expect("nine pages")
    };
    let take_400 = pages(small.clone(), 400);
    let take_4000 = pages(large.clone(), 4000);
    println!(
        "[1463] list_us 400={} 4000={} full_us 400={} 4000={} page_take_us 400={} 4000={}",
        at_400.as_micros(),
        at_4000.as_micros(),
        full_400.as_micros(),
        full_4000.as_micros(),
        take_400.as_micros(),
        take_4000.as_micros()
    );
    assert!(
        full_4000 > full_400 * BOUND,
        "the control: the full scene grows with the store ({full_400:?} to {full_4000:?})"
    );
    assert!(
        at_4000 < at_400 * BOUND,
        "the list's draw grew with the store: {at_400:?} at 400 entries, {at_4000:?} at 4000"
    );
    assert!(
        take_4000 < take_400 * BOUND + Duration::from_millis(1),
        "a page's take grew with the store: {take_400:?} at 400 entries, {take_4000:?} at 4000"
    );
}
