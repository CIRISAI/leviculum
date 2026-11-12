//! The budgeted prefix fetch over a throttled link, deterministic: the
//! clock is injected and the throttled fetcher advances it by bytes
//! over rate, so the budget arithmetic is asserted without a sleep.
//! Coarse pieces land first, the budget stops the fetch cleanly, a
//! resumed fetch continues where it stopped, and a tampered piece
//! fails the fetch with the good prefix intact.

mod common;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use leviculum_map::region::Region;
use leviculum_std::generate_identity;
use lnmapd::client::{fetch_prefix, Clock, FetchError, PieceFetcher};
use lnmapd::store::{add_pack, piece_path, AddOptions, Pack, VerifyError, KIND_VECTORS};

/// A clock the fetcher advances.
#[derive(Clone, Default)]
struct FakeClock(Rc<Cell<Duration>>);

impl Clock for FakeClock {
    fn elapsed(&self) -> Duration {
        self.0.get()
    }
}

/// A link that delivers `rate` bytes per second: each fetch advances
/// the clock by the piece's transfer time. One piece id may be
/// poisoned to arrive corrupted.
struct Throttled {
    pieces: HashMap<String, Vec<u8>>,
    rate: u64,
    clock: FakeClock,
    poisoned: Option<String>,
    served: Vec<String>,
}

impl PieceFetcher for Throttled {
    fn fetch_piece(&mut self, id: &str) -> Result<Vec<u8>, FetchError> {
        let mut bytes = self
            .pieces
            .get(id)
            .cloned()
            .ok_or_else(|| FetchError::Transport {
                id: id.to_owned(),
                reason: "unknown piece".to_owned(),
            })?;
        let secs = bytes.len() as f64 / self.rate as f64;
        self.clock
            .0
            .set(self.clock.elapsed() + Duration::from_secs_f64(secs));
        self.served.push(id.to_owned());
        if self.poisoned.as_deref() == Some(id) {
            bytes[0] ^= 1;
        }
        Ok(bytes)
    }
}

/// Build the store once and load its pieces into a throttled fetcher.
fn fixture(rate: u64) -> (Pack, Throttled, FakeClock) {
    let region = common::grunewald_region();
    let store = tempfile::tempdir().expect("store dir");
    let identity = generate_identity();
    let index = add_pack(
        store.path(),
        &region,
        &identity,
        &AddOptions {
            id: "grunewald".to_owned(),
            kind: KIND_VECTORS.to_owned(),
            http_url: None,
        },
    )
    .expect("add the pack");
    let pack = index.pack("grunewald").expect("pack").clone();
    let mut pieces = HashMap::new();
    for piece in &pack.pieces {
        let bytes = std::fs::read(piece_path(store.path(), &piece.id())).expect("read piece");
        pieces.insert(piece.id(), bytes);
    }
    let clock = FakeClock::default();
    let fetcher = Throttled {
        pieces,
        rate,
        clock: clock.clone(),
        poisoned: None,
        served: Vec::new(),
    };
    (pack, fetcher, clock)
}

#[test]
fn the_budget_stops_after_the_coarse_pieces_and_a_resume_finishes() {
    // Rate: the first two pieces together take exactly one second, so
    // a one-second budget lets piece 1 start (the clock is short of
    // the budget) and stops piece 2 (the clock has reached it).
    let (pack, mut fetcher, clock) = fixture(1);
    assert!(pack.pieces.len() >= 3, "need three pieces for this ladder");
    fetcher.rate = u64::from(pack.pieces[0].len) + u64::from(pack.pieces[1].len);

    let mut out = Vec::new();
    let fetched = fetch_prefix(
        &mut fetcher,
        &clock,
        &pack,
        Duration::from_secs(1),
        &mut out,
    )
    .expect("the budgeted fetch succeeds");

    assert_eq!(fetched.pieces, 2, "the budget stops before the fine piece");
    assert!(!fetched.complete);
    assert_eq!(fetched.zoom, Some(pack.pieces[1].zoom));
    assert_eq!(
        fetcher.served,
        vec![pack.pieces[0].id(), pack.pieces[1].id()],
        "pieces are asked for coarse to fine, nothing beyond the budget"
    );
    let prefix = Region::from_bytes(&out).expect("the stopped assembly is a readable region");
    assert!(!prefix.is_complete());

    // The next window picks up where this one stopped and finishes.
    let fetched = fetch_prefix(
        &mut fetcher,
        &clock,
        &pack,
        Duration::from_secs(3600),
        &mut out,
    )
    .expect("the resumed fetch succeeds");
    assert!(fetched.complete);
    assert_eq!(fetched.pieces, pack.pieces.len());
    assert_eq!(out.len() as u64, pack.total);
    let whole = Region::from_bytes(&out).expect("the finished assembly opens");
    assert!(whole.is_complete());
}

#[test]
fn the_first_piece_lands_even_when_the_budget_is_already_spent() {
    let (pack, mut fetcher, clock) = fixture(1);
    let mut out = Vec::new();
    let fetched = fetch_prefix(&mut fetcher, &clock, &pack, Duration::ZERO, &mut out)
        .expect("the fetch succeeds");
    assert_eq!(
        fetched.pieces, 1,
        "a view is never left with nothing: the coarse piece always lands"
    );
    Region::from_bytes(&out).expect("one piece is a readable region");
}

#[test]
fn a_tampered_piece_fails_the_fetch_and_keeps_the_good_prefix() {
    let (pack, mut fetcher, clock) = fixture(u64::MAX);
    fetcher.poisoned = Some(pack.pieces[1].id());

    let mut out = Vec::new();
    let error = fetch_prefix(
        &mut fetcher,
        &clock,
        &pack,
        Duration::from_secs(3600),
        &mut out,
    )
    .expect_err("a tampered piece must fail the fetch");
    assert!(
        matches!(
            error,
            FetchError::Verify(VerifyError::Tampered { reason: "hash", .. })
        ),
        "got {error:?}"
    );
    assert_eq!(
        out.len() as u64,
        u64::from(pack.pieces[0].len),
        "nothing of the tampered piece reaches the assembly"
    );
    Region::from_bytes(&out).expect("the good prefix is still a readable region");
}
