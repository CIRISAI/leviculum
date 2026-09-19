//! A stamp grind at a legal-but-large announced cost has to be abandonable
//! (Codeberg #185).
//!
//! The #181 ceiling removed cost 255, whose search cannot terminate at all.
//! Every cost from roughly forty bits up is just as unfinishable in practice
//! and is still inside the window the reference is willing to announce
//! (`LXMRouter.py:1042-1045`), so a peer can park our executor with a legal
//! announce. The bound cannot be tightened without refusing costs a
//! conforming peer may use, so the work is made cancellable instead — as the
//! reference does with `LXStamper.cancel_work`
//! (`reference/LXMF/LXMF/LXStamper.py:146-176`).
//!
//! The harness polls by hand instead of blocking on the future: a test that
//! proves non-termination must not itself hang, and a bounded poll count
//! turns "never finishes" into a failed assertion in under a second.
#![cfg(feature = "pow")]

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use leviculum_lxmf::{CooperativeStamper, StampCancel, StampError};

/// Drive `fut` for at most `polls` polls. With [`CooperativeStamper`]'s
/// `yield_every` set to 1 each poll is one candidate digest, so the count is
/// a direct effort budget.
fn poll_bounded<F: Future>(mut fut: Pin<&mut F>, polls: usize) -> Option<F::Output> {
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    for _ in 0..polls {
        if let Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
            return Some(value);
        }
    }
    None
}

/// The control. Without it a broken future would make the case below pass for
/// the wrong reason, and the budget it pins is what makes 254 meaningful.
#[test]
fn a_cost_the_search_can_reach_finishes_inside_the_budget() {
    let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
    stamper.yield_every = 1;
    let cancel = StampCancel::new();
    let future = stamper.generate(b"material", 12, 1, &cancel);
    futures::pin_mut!(future);
    assert!(
        poll_bounded(future, 200_000).is_some(),
        "cost 12 needs ~4096 candidates on average and must finish"
    );
}

/// The bug: at cost 254 the search produces no terminal result, ever. Before
/// the fix this was the whole story and the executor stayed parked.
#[test]
fn a_legal_but_large_cost_never_finishes_on_its_own() {
    let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
    stamper.yield_every = 1;
    let cancel = StampCancel::new();
    let future = stamper.generate(b"material", 254, 1, &cancel);
    futures::pin_mut!(future);
    assert!(
        poll_bounded(future, 200_000).is_none(),
        "cost 254 cannot be mined; a result here means the search was capped, \
         which would refuse a cost a conforming peer may announce"
    );
}

/// The fix: the same unfinishable grind ends on the caller's word, with a
/// named error rather than silence.
#[test]
fn cancelling_a_running_grind_ends_it_with_a_named_error() {
    let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
    stamper.yield_every = 1;
    let cancel = StampCancel::new();
    let future = stamper.generate(b"material", 254, 1, &cancel);
    futures::pin_mut!(future);

    // Running, not merely queued: the handle has to be observed by a search
    // already in its loop, which is the case the executor gets parked in.
    assert!(poll_bounded(future.as_mut(), 1_000).is_none());

    cancel.cancel();
    assert_eq!(
        poll_bounded(future, 4),
        Some(Err(StampError::Cancelled)),
        "cancellation is observed within one yield interval"
    );
}

/// A handle already fired before the job starts spends no work at all — the
/// case a host hits when a message is cancelled between queueing the mine and
/// the worker picking it up.
#[test]
fn a_grind_cancelled_before_it_starts_does_no_work() {
    let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
    let cancel = StampCancel::new();
    cancel.cancel();
    // 3000 expansion rounds: had the workblock been built first, one poll
    // could not have reached the answer.
    let future = stamper.generate(b"material", 254, 3_000, &cancel);
    futures::pin_mut!(future);
    assert_eq!(poll_bounded(future, 1), Some(Err(StampError::Cancelled)));
}
