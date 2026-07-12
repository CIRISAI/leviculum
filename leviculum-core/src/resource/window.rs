//! Receiver-side resource sliding-window state and adaptation policy.
//!
//! Codeberg #85: the window adaptation logic is extracted out of
//! [`IncomingResource`](super::incoming::IncomingResource) into a swappable
//! [`WindowPolicy`] so candidate algorithms can be benchmarked against each
//! other in the same harness. The only policy today is [`WindowPolicy::Current`],
//! which reproduces the historical behavior exactly.

use crate::constants::{
    RESOURCE_WINDOW_INITIAL, RESOURCE_WINDOW_MAX_FAST, RESOURCE_WINDOW_MAX_SLOW,
};
use crate::resource::{
    FAST_RATE_THRESHOLD, RESOURCE_WINDOW_FLEXIBILITY, RESOURCE_WINDOW_MAX_VERY_SLOW,
    SLOW_RATE_THRESHOLD, VERY_SLOW_RATE_THRESHOLD,
};

/// Receiver-side rate measurements for one completed window round.
///
/// `first_part_rate` is the classic eifr quantity: bytes_per_part * 1000 /
/// elapsed_ms between sending the REQ and the first part of the window
/// arriving. `round_rate` is the whole-round goodput: total bytes received in
/// this window divided by the round's elapsed time. Policies choose which
/// measurement to react to; [`WindowPolicy::Current`] uses `first_part_rate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateSample {
    /// bytes_per_part * 1000 / ms from REQ to the window's first part (B/s).
    pub first_part_rate: u64,
    /// Bytes received this window * 1000 / round elapsed ms (B/s).
    pub round_rate: u64,
}

/// Selects the receive-window adaptation algorithm (Codeberg #85).
///
/// Only `Current` exists in this batch; candidate algorithms are added as
/// further variants and selected via the `LEVICULUM_RESOURCE_WINDOW_POLICY`
/// environment variable in `lnsd` and `lncp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowPolicy {
    /// The historical algorithm: grow the window by 1 once
    /// `window + FLEXIBILITY` consecutive rounds completed, pick the
    /// window_max tier from the first-part rate, clamp down on tier drop.
    /// Timeouts never touch the window.
    #[default]
    Current,
}

impl WindowPolicy {
    /// Parse a policy name as used by `LEVICULUM_RESOURCE_WINDOW_POLICY`.
    /// Returns `None` for unknown names so callers can warn and fall back.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "current" => Some(Self::Current),
            _ => None,
        }
    }
}

/// The receive-window state moved out of `IncomingResource`.
///
/// All mutation of the window happens here, driven by the owning
/// `IncomingResource` at three points: parts arriving (`record_part`), the
/// round completing (`on_round_complete`), and the request timing out
/// (`on_timeout`).
pub(crate) struct WindowState {
    /// Current window size: number of parts requested per REQ.
    window: usize,
    /// Ceiling the window may grow to, selected by rate tier.
    window_max: usize,
    /// Parts received since the current REQ was built.
    parts_received_this_window: usize,
    /// Bytes received since the current REQ was built (for round_rate).
    bytes_received_this_window: u64,
    /// Rounds completed since the window last grew.
    consecutive_completed_windows: usize,
    /// Total completed rounds over the transfer (observability only).
    rounds_completed: usize,
}

impl WindowState {
    pub(crate) fn new() -> Self {
        Self {
            window: RESOURCE_WINDOW_INITIAL,
            window_max: RESOURCE_WINDOW_MAX_SLOW,
            parts_received_this_window: 0,
            bytes_received_this_window: 0,
            consecutive_completed_windows: 0,
            rounds_completed: 0,
        }
    }

    pub(crate) fn window(&self) -> usize {
        self.window
    }

    pub(crate) fn window_max(&self) -> usize {
        self.window_max
    }

    pub(crate) fn parts_received_this_window(&self) -> usize {
        self.parts_received_this_window
    }

    pub(crate) fn bytes_received_this_window(&self) -> u64 {
        self.bytes_received_this_window
    }

    pub(crate) fn rounds_completed(&self) -> usize {
        self.rounds_completed
    }

    /// A part of the current window arrived.
    pub(crate) fn record_part(&mut self, bytes: usize) {
        self.parts_received_this_window += 1;
        self.bytes_received_this_window += bytes as u64;
    }

    /// A new REQ was built: the per-round counters restart.
    pub(crate) fn start_round(&mut self) {
        self.parts_received_this_window = 0;
        self.bytes_received_this_window = 0;
    }

    /// All outstanding parts of the current window arrived.
    pub(crate) fn on_round_complete(&mut self, policy: WindowPolicy, sample: RateSample) {
        self.rounds_completed += 1;
        match policy {
            WindowPolicy::Current => self.round_complete_current(sample.first_part_rate),
        }
    }

    /// The part request timed out and is being retransmitted.
    pub(crate) fn on_timeout(&mut self, policy: WindowPolicy) {
        match policy {
            // The historical logic never touches the window on timeout.
            WindowPolicy::Current => {}
        }
    }

    /// The historical round-complete logic, verbatim: bump the completed-round
    /// counter, grow the window by 1 once it reaches window + FLEXIBILITY,
    /// then pick the window_max tier from the first-part rate, clamping the
    /// window down if it exceeds a lowered window_max.
    fn round_complete_current(&mut self, rate: u64) {
        self.consecutive_completed_windows += 1;

        if self.consecutive_completed_windows >= self.window + RESOURCE_WINDOW_FLEXIBILITY {
            if self.window < self.window_max {
                self.window += 1;
            }
            self.consecutive_completed_windows = 0;
        }

        if rate > FAST_RATE_THRESHOLD {
            self.window_max = RESOURCE_WINDOW_MAX_FAST;
        } else if rate < VERY_SLOW_RATE_THRESHOLD {
            self.window_max = RESOURCE_WINDOW_MAX_VERY_SLOW;
            if self.window > self.window_max {
                self.window = self.window_max;
            }
        } else if rate < SLOW_RATE_THRESHOLD {
            self.window_max = RESOURCE_WINDOW_MAX_SLOW;
            if self.window > self.window_max {
                self.window = self.window_max;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(first_part_rate: u64) -> RateSample {
        RateSample {
            first_part_rate,
            round_rate: 0,
        }
    }

    #[test]
    fn test_initial_state() {
        let ws = WindowState::new();
        assert_eq!(ws.window(), RESOURCE_WINDOW_INITIAL);
        assert_eq!(ws.window_max(), RESOURCE_WINDOW_MAX_SLOW);
        assert_eq!(ws.parts_received_this_window(), 0);
        assert_eq!(ws.rounds_completed(), 0);
    }

    #[test]
    fn test_current_growth_cadence() {
        // window grows by 1 after window + FLEXIBILITY completed rounds.
        let mut ws = WindowState::new();
        let rate = SLOW_RATE_THRESHOLD; // mid tier: window_max untouched
        for _ in 0..(RESOURCE_WINDOW_INITIAL + RESOURCE_WINDOW_FLEXIBILITY - 1) {
            ws.on_round_complete(WindowPolicy::Current, sample(rate));
            assert_eq!(ws.window(), RESOURCE_WINDOW_INITIAL);
        }
        ws.on_round_complete(WindowPolicy::Current, sample(rate));
        assert_eq!(ws.window(), RESOURCE_WINDOW_INITIAL + 1);
    }

    #[test]
    fn test_current_tier_selection_and_clamp() {
        let mut ws = WindowState::new();
        ws.on_round_complete(WindowPolicy::Current, sample(FAST_RATE_THRESHOLD + 1));
        assert_eq!(ws.window_max(), RESOURCE_WINDOW_MAX_FAST);
        // Grow the window past the very-slow ceiling, then drop the tier.
        while ws.window() <= RESOURCE_WINDOW_MAX_VERY_SLOW {
            ws.on_round_complete(WindowPolicy::Current, sample(FAST_RATE_THRESHOLD + 1));
        }
        ws.on_round_complete(WindowPolicy::Current, sample(VERY_SLOW_RATE_THRESHOLD - 1));
        assert_eq!(ws.window_max(), RESOURCE_WINDOW_MAX_VERY_SLOW);
        assert_eq!(ws.window(), RESOURCE_WINDOW_MAX_VERY_SLOW);
    }

    #[test]
    fn test_current_mid_tier_keeps_window_max() {
        let mut ws = WindowState::new();
        ws.on_round_complete(WindowPolicy::Current, sample(FAST_RATE_THRESHOLD + 1));
        assert_eq!(ws.window_max(), RESOURCE_WINDOW_MAX_FAST);
        // Between SLOW and FAST thresholds the tier is left as-is.
        ws.on_round_complete(WindowPolicy::Current, sample(SLOW_RATE_THRESHOLD));
        assert_eq!(ws.window_max(), RESOURCE_WINDOW_MAX_FAST);
    }

    #[test]
    fn test_current_timeout_is_noop() {
        let mut ws = WindowState::new();
        ws.on_round_complete(WindowPolicy::Current, sample(FAST_RATE_THRESHOLD + 1));
        let (w, m) = (ws.window(), ws.window_max());
        ws.on_timeout(WindowPolicy::Current);
        assert_eq!((ws.window(), ws.window_max()), (w, m));
    }

    #[test]
    fn test_round_counters() {
        let mut ws = WindowState::new();
        ws.record_part(464);
        ws.record_part(464);
        assert_eq!(ws.parts_received_this_window(), 2);
        assert_eq!(ws.bytes_received_this_window(), 928);
        ws.start_round();
        assert_eq!(ws.parts_received_this_window(), 0);
        assert_eq!(ws.bytes_received_this_window(), 0);
    }

    #[test]
    fn test_policy_parse() {
        assert_eq!(WindowPolicy::parse("current"), Some(WindowPolicy::Current));
        assert_eq!(
            WindowPolicy::parse(" Current "),
            Some(WindowPolicy::Current)
        );
        assert_eq!(WindowPolicy::parse("bogus"), None);
        assert_eq!(WindowPolicy::parse(""), None);
    }
}
