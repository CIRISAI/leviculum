//! A 64-bit counter that exists on targets without a 64-bit atomic
//! (Codeberg #415).
//!
//! `std::sync::atomic::AtomicU64` is compiled only where
//! `target_has_atomic = "64"`. A 32-bit MIPS router (`mips-unknown-linux-musl`,
//! the target in #415) has atomics for 8, 16, 32 and ptr and nothing wider, so
//! every `AtomicU64` in this crate is an unresolved import there and `lnsd`
//! does not build at all.
//!
//! Narrowing the counters to `AtomicUsize` is the fix that suggests itself and
//! it is wrong: on a 32-bit target `rx_bytes`/`tx_bytes` then wrap at 4 GiB,
//! and a router that has been up for a week reports totals that silently
//! restarted. These are byte counters read by `rnstatus`; they must count to
//! 2^64 on every target we build for.
//!
//! So the width is fixed and the mechanism varies: [`AtomicCounter64`] where
//! the target has the instruction, [`MutexCounter64`] where it does not, and
//! [`Counter64`] selecting between them. Both carry the `AtomicU64` method
//! shapes — `new`/`load`/`store`/`fetch_add`/`swap`, `Ordering` included — so a
//! call site reads the same on either arm and the choice stays inside this
//! module.
//!
//! The cost is one uncontended lock per counted frame on the fallback arm.
//! That is what a 32-bit router pays to keep its byte totals; hosts with the
//! instruction pay nothing, because they never compile the fallback in.
//!
//! # Why both types are always compiled
//!
//! `MutexCounter64` is built on every target, including the 64-bit hosts that
//! never select it, so its unit tests run on every gate we have. A fallback
//! that compiled only on MIPS would be exercised only by a cross-build that
//! checks, never runs — which is to say never.

use std::sync::atomic::Ordering;
use std::sync::Mutex;

use crate::sync_ext::MutexRecover;

/// The counter type this crate uses. `AtomicU64` where the target has a
/// 64-bit atomic, a mutex-guarded `u64` where it does not.
#[cfg(target_has_atomic = "64")]
pub(crate) type Counter64 = AtomicCounter64;

/// The counter type this crate uses. `AtomicU64` where the target has a
/// 64-bit atomic, a mutex-guarded `u64` where it does not.
#[cfg(not(target_has_atomic = "64"))]
pub(crate) type Counter64 = MutexCounter64;

/// The lock-free arm: a plain `AtomicU64`, one instruction per update.
#[cfg(target_has_atomic = "64")]
#[derive(Debug)]
pub(crate) struct AtomicCounter64(std::sync::atomic::AtomicU64);

#[cfg(target_has_atomic = "64")]
impl AtomicCounter64 {
    /// A counter starting at `value`. `const` so the counter can back a
    /// `static` (see `event_log::VISITS`).
    pub(crate) const fn new(value: u64) -> Self {
        Self(std::sync::atomic::AtomicU64::new(value))
    }

    pub(crate) fn load(&self, order: Ordering) -> u64 {
        self.0.load(order)
    }

    pub(crate) fn store(&self, value: u64, order: Ordering) {
        self.0.store(value, order)
    }

    pub(crate) fn fetch_add(&self, value: u64, order: Ordering) -> u64 {
        self.0.fetch_add(value, order)
    }

    pub(crate) fn swap(&self, value: u64, order: Ordering) -> u64 {
        self.0.swap(value, order)
    }
}

/// The fallback arm: a `u64` behind a mutex, selected as [`Counter64`] only
/// where the target lacks a 64-bit atomic.
///
/// Compiled everywhere so the tests below cover it on ordinary hosts; hence
/// the `allow(dead_code)`, which is what "unused on 64-bit targets" looks
/// like to the compiler.
///
/// The `Ordering` arguments are accepted and ignored. Every access here is
/// inside the mutex's acquire/release pair, which is at least as strong as
/// the `Relaxed` this crate's counters ask for. It is not a general
/// `AtomicU64` substitute: a caller that needs `SeqCst` participation in one
/// global total order with *other* atomics does not get it from a lock.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct MutexCounter64(Mutex<u64>);

#[allow(dead_code)]
impl MutexCounter64 {
    /// A counter starting at `value`. `const` for the same reason as the
    /// atomic arm — `Mutex::new` is a `const fn`.
    pub(crate) const fn new(value: u64) -> Self {
        Self(Mutex::new(value))
    }

    pub(crate) fn load(&self, _order: Ordering) -> u64 {
        *self.0.lock_recover()
    }

    pub(crate) fn store(&self, value: u64, _order: Ordering) {
        *self.0.lock_recover() = value;
    }

    /// Returns the value *before* the add, like `AtomicU64::fetch_add`.
    ///
    /// Wrapping on overflow is also `AtomicU64`'s documented behaviour, and
    /// at one byte per nanosecond 2^64 is 584 years away.
    pub(crate) fn fetch_add(&self, value: u64, _order: Ordering) -> u64 {
        let mut guard = self.0.lock_recover();
        let previous = *guard;
        *guard = previous.wrapping_add(value);
        previous
    }

    pub(crate) fn swap(&self, value: u64, _order: Ordering) -> u64 {
        let mut guard = self.0.lock_recover();
        std::mem::replace(&mut *guard, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect the reporter's `AtomicUsize` workaround would have shipped:
    /// a byte counter on a 32-bit target restarting at 4 GiB. Run against the
    /// fallback arm, which is the one a 32-bit target compiles.
    #[test]
    fn mutex_counter_keeps_counting_past_2_pow_32() {
        let counter = MutexCounter64::new(0);
        // 4 GiB in chunks a real interface would hand it, plus one more.
        let chunk = 1 << 20;
        for _ in 0..4096 {
            counter.fetch_add(chunk, Ordering::Relaxed);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 1 << 32);
        counter.fetch_add(chunk, Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), (1u64 << 32) + chunk);
    }

    /// Same assertion on whichever arm this target actually selects, so the
    /// property is checked through the alias the crate uses and not only
    /// through the type the test names.
    #[test]
    fn selected_counter_keeps_counting_past_2_pow_32() {
        let counter = Counter64::new((1u64 << 32) - 1);
        counter.fetch_add(2, Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), (1u64 << 32) + 1);
    }

    #[test]
    fn mutex_counter_fetch_add_swap_and_store_match_the_atomic() {
        let counter = MutexCounter64::new(7);
        assert_eq!(counter.fetch_add(5, Ordering::Relaxed), 7);
        assert_eq!(counter.load(Ordering::Relaxed), 12);
        assert_eq!(counter.swap(0, Ordering::Relaxed), 12);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        counter.store(1u64 << 33, Ordering::Release);
        assert_eq!(counter.load(Ordering::Acquire), 1u64 << 33);
    }

    /// `AtomicU64::fetch_add` wraps on overflow rather than panicking; the
    /// fallback has to do the same, in release and in debug, where a plain
    /// `+=` would trap on the overflow check.
    #[test]
    fn mutex_counter_wraps_like_the_atomic() {
        let counter = MutexCounter64::new(u64::MAX);
        assert_eq!(counter.fetch_add(2, Ordering::Relaxed), u64::MAX);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    /// Concurrent adds land: the lock is what makes the fallback a counter
    /// and not a read-modify-write race.
    #[test]
    fn mutex_counter_counts_across_threads() {
        let counter = std::sync::Arc::new(MutexCounter64::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let counter = std::sync::Arc::clone(&counter);
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for t in threads {
            assert!(t.join().is_ok());
        }
        assert_eq!(counter.load(Ordering::Relaxed), 40_000);
    }

    /// A `static` is how `event_log::VISITS` holds one, so `new` has to stay
    /// usable in a const context on both arms.
    #[test]
    fn counter_is_const_constructible() {
        static SELECTED: Counter64 = Counter64::new(0);
        static FALLBACK: MutexCounter64 = MutexCounter64::new(0);
        SELECTED.fetch_add(3, Ordering::Relaxed);
        FALLBACK.fetch_add(4, Ordering::Relaxed);
        assert_eq!(SELECTED.load(Ordering::Relaxed), 3);
        assert_eq!(FALLBACK.load(Ordering::Relaxed), 4);
    }
}
