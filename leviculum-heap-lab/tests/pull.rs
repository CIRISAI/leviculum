//! 1569: the pull starves the M9's UI core in the allocator, not in the
//! fold.
//!
//! Window 16a (bench image of 9f53b300, 4266 entries re-pulled): core
//! 0's host stage grew from 170 ms to 2473 ms a turn and core 1's turn
//! (read off the flush of a single 12,800-byte DMA chunk) from 5 ms to
//! 3369 ms, both falling back to the baseline after each watchdog
//! reset — the store unchanged, only the live heap emptied. The fold is
//! flat on the host under the system allocator
//! (`loquax-m9/tests/turn.rs`, 1463). What both cores share and a reset
//! empties is the heap: the firmware's `embedded_alloc::LlffHeap`
//! (before 1569) walks one free list on every alloc and every free,
//! under the one cross-core critical section.
//!
//! This test runs the same pull — `loquax_m9::home::take`, page after
//! page — under both heaps `embedded_alloc` offers, on the host
//! (`leviculum-heap-lab`): the walk's growth is the data structure's,
//! so the host clock shows it too, only smaller. Red before 1569's fix
//! (the board linked llff; a page's take grows with the model), green
//! with the tlsf choice the firmware links now
//! (`leviculum-esp/src/lib.rs`).

use std::time::{Duration, Instant};

use leviculum_core::Identity;
use leviculum_heap_lab::{spilled, with, Which};
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::home;
use rand_core::OsRng;

/// The page the window's pull carried (`[HOME] page entries=55`).
const PAGE: usize = 55;

/// The archive, the shape of `loquax-m9/tests/turn.rs` (1463): twelve
/// friends, one message in five ours.
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
}

/// The model after the first `n` of `all` came in pages, and the cost
/// of one more page then: the least of nine, so a busy host only makes
/// every number slower, never one of them faster.
fn pulled(archive: &Archive, all: &[Entry], n: usize) -> (UiModel<Field>, Duration) {
    let mut model = home::model(archive.address);
    for page in all[..n].chunks(PAGE) {
        home::take(&mut model, page, &archive.address);
    }
    let cost = (0..9)
        .map(|i| {
            let page = &all[n + i * PAGE..n + (i + 1) * PAGE];
            let t = Instant::now();
            home::take(&mut model, page, &archive.address);
            t.elapsed()
        })
        .min()
        .unwrap_or_default();
    (model, cost)
}

#[test]
fn a_pulled_page_costs_the_same_at_4000_entries_as_at_400() {
    let archive = Archive::new();
    // Packed under the system allocator: the home packs, the board only
    // unpacks, so only the take's own churn lands in the lab heaps.
    let all = archive.entries(1, 4001 + 9 * PAGE as u64);
    let lab = |which| {
        let before = spilled();
        let out = with(which, || {
            let (small, at_400) = pulled(&archive, &all, 400);
            let (large, at_4000) = pulled(&archive, &all, 4000);
            drop((small, large));
            (at_400, at_4000)
        });
        assert_eq!(spilled(), before, "the lab heap overflowed its region");
        out
    };
    let (tlsf_400, tlsf_4000) = lab(Which::Tlsf);
    let (llff_400, llff_4000) = lab(Which::Llff);
    println!(
        "[1569] page_take_us llff 400={} 4000={} tlsf 400={} 4000={}",
        llff_400.as_micros(),
        llff_4000.as_micros(),
        tlsf_400.as_micros(),
        tlsf_4000.as_micros()
    );
    // The positive control: the walk the board starved in is real and
    // this workload shows it, or the test could not catch its return.
    assert!(
        llff_4000 > llff_400 * 3,
        "the llff walk stopped growing: {llff_400:?} at 400 entries, {llff_4000:?} at 4000 \
         — the control is gone, re-measure the lab"
    );
    // The property the board needs (1569): under the heap the firmware
    // links, a pulled page costs the same into a full model as into a
    // young one.
    assert!(
        tlsf_4000 < tlsf_400 * 3 + Duration::from_millis(1),
        "a page's take grew with the model under tlsf: {tlsf_400:?} at 400 entries, \
         {tlsf_4000:?} at 4000"
    );
}
