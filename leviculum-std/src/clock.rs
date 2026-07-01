//! System clock implementation for std platforms
//!
//! Provides a monotonic clock using `std::time::Instant`.

use leviculum_core::traits::Clock;
use std::time::Instant;

/// System clock using `std::time::Instant`
///
/// Monotonic, suitable for timeouts and RTT measurement.
pub(crate) struct SystemClock {
    start: Instant,
}

impl SystemClock {
    /// Create a new system clock (epoch = now)
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Expose the start anchor so the interface layer's backpressure
    /// clock can align with ours. The airtime bucket's `last_update_ms`
    /// and `Transport::now_ms` must share a frame, otherwise the
    /// retry-scheduler's deferral math breaks.
    pub(crate) fn start_instant(&self) -> Instant {
        self.start
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_clock() {
        let clock = SystemClock::new();
        let t1 = clock.now_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = clock.now_ms();
        assert!(t2 > t1);
    }

    #[test]
    fn test_clock_trait_methods() {
        let clock = SystemClock::new();
        let deadline = clock.deadline(1000);
        assert!(!clock.has_elapsed(deadline));
        assert!(clock.now_secs() < 1); // Just started
    }
}
