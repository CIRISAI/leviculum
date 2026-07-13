//! Poison-tolerant locking for the daemon's `std::sync::Mutex` state.
//!
//! The whole node state (`StdNodeCore`) and several interface-side maps live
//! behind `std::sync::Mutex`. A `std::sync::Mutex` POISONS: if any task panics
//! while holding the guard, every subsequent `.lock().unwrap()` panics too,
//! turning one isolated task panic into a whole-daemon crash cascade.
//!
//! For a long-running network daemon that must not fall over because one
//! interface had a bad moment, continuing degraded beats crashing. This trait
//! recovers the guard on poison instead of propagating the panic.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock a `std::sync::Mutex`, recovering the guard if the mutex is poisoned.
pub(crate) trait MutexRecover<T> {
    /// Acquire the lock, ignoring poison.
    ///
    /// Trade-off: on poison we return the guard anyway
    /// (via [`PoisonError::into_inner`]), so the protected state may reflect a
    /// partially-applied mutation from the task that panicked while holding the
    /// lock. We accept that because for a daemon, continuing degraded beats
    /// crashing every later locker — and the panic that poisoned the mutex has
    /// already unwound and logged on its own task. Use this instead of
    /// `.lock().unwrap()` / `.lock().expect(...)` on every non-test std mutex.
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexRecover<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn lock_recover_returns_usable_guard_after_poison() {
        let m = Arc::new(Mutex::new(41_u32));

        // Poison the mutex: a thread panics while holding the guard.
        let poisoner = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let mut g = poisoner.lock().unwrap();
            *g = 1; // partially-applied mutation before the panic
            panic!("poison");
        })
        .join();

        // RED-before: a plain `.lock().unwrap()` panics on the poisoned mutex.
        let plain = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = m.lock().unwrap();
        }));
        assert!(
            plain.is_err(),
            "plain lock().unwrap() must panic when poisoned"
        );

        // GREEN-after: `lock_recover()` returns a usable guard, no panic, and
        // the value is still readable and writable.
        let mut g = m.lock_recover();
        let observed = *g;
        *g = 99;
        drop(g);
        assert_eq!(observed, 1, "recovered guard exposes the poisoned state");
        assert_eq!(*m.lock_recover(), 99, "recovered guard is writable");
    }

    #[test]
    fn lock_recover_matches_lock_when_unpoisoned() {
        let m = Mutex::new(7_u32);
        {
            let mut g = m.lock_recover();
            assert_eq!(*g, 7);
            *g = 8;
        }
        assert_eq!(*m.lock().unwrap(), 8);
    }
}
