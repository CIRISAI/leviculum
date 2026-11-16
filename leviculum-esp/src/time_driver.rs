//! embassy-time on the ESP firmware without an executor (1478b).
//!
//! `leviculum-lr11xx` is async on `embassy-time` (its `Timer` and
//! `with_timeout`), and the ESP images run no executor: core 0 is one loop
//! (`thinknode_m9.rs`), which polls the radio's task once a turn with a
//! no-op waker ([`poll_once`]), as `leviculum-nrf`'s frame writer polls
//! eagerly. A timer here therefore needs only the clock: a future that is
//! not due is polled again on the loop's next turn, at most one keyboard
//! tick (10 ms) away, so [`Driver::schedule_wake`] keeps no queue and
//! wakes nothing.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use embassy_time_driver::{Driver, TICK_HZ};

use crate::uptime_us;

// The driver counts microseconds, embassy-time's default tick; a build
// that chose another tick rate would read every timer wrong.
const _: () = assert!(TICK_HZ == 1_000_000);

/// The clock of [`crate::uptime_us`], esp-hal's `Instant` since boot.
struct EspClock;

impl Driver for EspClock {
    fn now(&self) -> u64 {
        uptime_us()
    }

    fn schedule_wake(&self, _at: u64, _waker: &Waker) {
        // Nothing to arm: the caller polls every turn.
    }
}

embassy_time_driver::time_driver_impl!(static DRIVER: EspClock = EspClock);

/// Poll `fut` once with a waker that does nothing.
pub fn poll_once<F: Future + ?Sized>(fut: Pin<&mut F>) -> Poll<F::Output> {
    let mut cx = Context::from_waker(Waker::noop());
    fut.poll(&mut cx)
}

/// A future that is pending once, then done: the task's yield back to the
/// loop when the bus is not its own.
pub struct YieldOnce(bool);

impl YieldOnce {
    pub fn new() -> Self {
        Self(false)
    }
}

impl Default for YieldOnce {
    fn default() -> Self {
        Self::new()
    }
}

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}
