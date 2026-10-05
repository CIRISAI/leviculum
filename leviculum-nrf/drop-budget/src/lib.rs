//! How many per-drop `[DROP]` lines the debug CDC may carry (Codeberg #346).
//!
//! Since #346 the core raises a `NodeEvent::PacketDropped` for every packet
//! the receive path throws away, and the boards render one line per event.
//! That stream is deliberately the high-volume one: on a shared medium a
//! transport node hears every packet routed via its neighbours, so
//! `overheard-transport-id` is the commonest drop there is. Unlimited, a
//! quiet evening's capture would be a wall of them.
//!
//! # What the limit is sized against
//!
//! Not the USB link. `debug_writer_task` drains `LOG_RING` completely on
//! every wake (`leviculum-nrf/src/usb.rs`, the inner `loop` around
//! `LOG_RING.read`), so a host with the port open is not the bottleneck.
//!
//! The bound that binds is the **ring**: `LOG_RING_SIZE` is 8 KiB
//! (`leviculum-nrf/src/log.rs:25`) and old data is overwritten when it fills.
//! A drop storm therefore does not lose drop lines — it evicts the OTHER
//! lines a capture was taken for. Since #421 put the 16-hex `ph=` correlator
//! on the line, one `[DROP]` line is up to ~90 bytes (the worst case is
//! rendered, not estimated, in `the_ring_derivation_still_holds` below).
//! The periodic traffic a reader needs beside the drops — `[STACK]` every
//! 2 s, `[FW_BUILD]` every 5 s, `[TRANSPORT]` every 30 s — is about 2.3 KiB
//! per 30 s window.
//!
//! [`LINES_PER_WINDOW`] = 2 per second keeps a full storm's worth of drop
//! lines (60 × ~90 B ≈ 5.4 KiB per 30 s) plus that context inside the 8 KiB
//! ring, so a full half-minute of context survives beside the storm. At
//! ~180 B/s it is also well below the 8 KiB the ring would need refilled per
//! second to lap itself. (Until #421 the line had no `ph=` and the budget
//! was 3; the wider line is the better trade — the whole point of the line
//! is naming the packet, and the summary still recovers the rate.)
//!
//! The LoRa side bounds the storm from the other direction: at the
//! project's default PHY (SF8, BW 125 kHz) a 100-byte frame is ~164 ms of
//! airtime, so one carrier delivers at most ~6 packets per second. Two
//! lines per second names a third of the worst a single radio can produce
//! and summarises the rest — while a BLE peer, which has no such ceiling,
//! is exactly the case the limiter earns its keep on.
//!
//! # Why a summary and not silence
//!
//! A suppressed line that leaves no trace turns a storm into a lull: the
//! capture shows three drops per second whatever the load, and the shape of
//! the event — which is the whole diagnostic — is gone. The window therefore
//! closes with one `[DROP] suppressed=<n> window_ms=<w>` line naming what it
//! held back, so the reader can always recover the rate.
//!
//! # Why a crate
//!
//! Same reason as `leviculum-queue-budget` and `leviculum-announce-policy`:
//! the firmware crate only builds for `thumbv7em`, so a policy tested only
//! there is tested nowhere. The window arithmetic — the boundary, the reset,
//! the clock that goes backwards — is exactly the part that has to be driven
//! against a fake clock on the host.

#![cfg_attr(not(test), no_std)]

/// The accounting window, in milliseconds.
///
/// One second: short enough that the summary names a rate a reader can use
/// directly, long enough that a two- or three-packet burst — a path response
/// plus an announce plus a link proof, which the stack produces together —
/// passes through whole instead of being clipped by a window boundary.
pub const WINDOW_MS: u32 = 1_000;

/// How many `[DROP]` lines may be printed per [`WINDOW_MS`] window.
///
/// See the module docs for the derivation from the 8 KiB `LOG_RING` and the
/// periodic lines a capture needs to keep beside the drops;
/// `the_ring_derivation_still_holds` pins the arithmetic against the real
/// rendered worst-case line.
pub const LINES_PER_WINDOW: u32 = 2;

/// What one closed window held back.
///
/// Carries [`WINDOW_MS`] rather than letting the caller supply it, so the
/// number on the line and the number the budget actually enforced cannot
/// drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suppressed {
    /// Lines the window refused. Never zero — a window that suppressed
    /// nothing produces no summary at all.
    pub lines: u32,
    /// The window those lines were refused over.
    pub window_ms: u32,
}

/// What the budget says about one drop.
///
/// Both fields can be set at once: a drop that arrives after a window has
/// expired closes that window (producing its summary) and is then admitted
/// into the fresh one. The caller prints the summary FIRST, so the capture
/// reads in the order the events happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The summary for a window this call closed, when that window held
    /// anything back.
    pub summary: Option<Suppressed>,
    /// Whether the caller prints this drop's own line.
    pub emit: bool,
}

/// The board's one budget for `[DROP]` lines.
///
/// A fixed window rather than a token bucket: the summary line has to name a
/// window for its count to mean anything, and a bucket has no window to name.
///
/// State is four words — a `u64` start, two `u32` counters and a flag — which
/// is the whole `.bss` cost of the feature.
#[derive(Debug)]
pub struct DropBudget {
    window_start_ms: u64,
    emitted: u32,
    suppressed: u32,
    open: bool,
}

impl DropBudget {
    /// A budget with no window running.
    ///
    /// The first drop opens the first window at its own timestamp, so the
    /// board's uptime at boot is never mistaken for a window that has already
    /// been running since `t=0`.
    pub const fn new() -> Self {
        Self {
            window_start_ms: 0,
            emitted: 0,
            suppressed: 0,
            open: false,
        }
    }

    /// Account for one drop and say whether its line is printed.
    pub fn admit(&mut self, now_ms: u64) -> Decision {
        let summary = self.close_if_expired(now_ms);
        if !self.open {
            self.open = true;
            self.window_start_ms = now_ms;
        }
        let emit = self.emitted < LINES_PER_WINDOW;
        if emit {
            self.emitted += 1;
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
        }
        Decision { summary, emit }
    }

    /// Close an expired window even though no further drop arrived.
    ///
    /// A storm that stops dead would otherwise hold its own summary hostage
    /// until the next drop, which on a mesh that just went quiet could be
    /// never — and the silence after a storm is precisely the reading the
    /// summary is for. The main loop calls this once per iteration.
    pub fn flush(&mut self, now_ms: u64) -> Option<Suppressed> {
        self.close_if_expired(now_ms)
    }

    fn close_if_expired(&mut self, now_ms: u64) -> Option<Suppressed> {
        if !self.open {
            return None;
        }
        // `saturating_sub`, not a subtraction: the board's clock is an uptime
        // that only goes forward today, but a budget that panicked or wrapped
        // on a backwards step would take the whole log path down with it. A
        // backwards step just leaves the window open.
        if now_ms.saturating_sub(self.window_start_ms) < u64::from(WINDOW_MS) {
            return None;
        }
        let lines = self.suppressed;
        self.open = false;
        self.emitted = 0;
        self.suppressed = 0;
        if lines == 0 {
            None
        } else {
            Some(Suppressed {
                lines,
                window_ms: WINDOW_MS,
            })
        }
    }
}

impl Default for DropBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admit_n(budget: &mut DropBudget, now_ms: u64, n: u32) -> (u32, u32) {
        let (mut emitted, mut suppressed) = (0, 0);
        for _ in 0..n {
            if budget.admit(now_ms).emit {
                emitted += 1;
            } else {
                suppressed += 1;
            }
        }
        (emitted, suppressed)
    }

    // The limit itself: the first LINES_PER_WINDOW drops in a window print,
    // every later one in that window does not.
    #[test]
    fn the_window_admits_exactly_the_limit() {
        let mut budget = DropBudget::new();
        let (emitted, suppressed) = admit_n(&mut budget, 10_000, LINES_PER_WINDOW + 7);
        assert_eq!(emitted, LINES_PER_WINDOW);
        assert_eq!(suppressed, 7);
    }

    // A quiet board never hits the limit, whatever its uptime: the first drop
    // opens the window at its own timestamp rather than inheriting one that
    // has notionally been running since t=0.
    #[test]
    fn the_first_drop_after_a_long_uptime_prints() {
        let mut budget = DropBudget::new();
        let first = budget.admit(9_000_000);
        assert!(first.emit);
        assert_eq!(first.summary, None, "no window closed, so nothing to say");
    }

    // The next window is a clean slate.
    #[test]
    fn a_new_window_restores_the_full_budget() {
        let mut budget = DropBudget::new();
        admit_n(&mut budget, 10_000, LINES_PER_WINDOW + 5);
        let (emitted, _) = admit_n(
            &mut budget,
            10_000 + u64::from(WINDOW_MS),
            LINES_PER_WINDOW + 2,
        );
        assert_eq!(emitted, LINES_PER_WINDOW);
    }

    // The summary rides the first drop of the NEXT window, and names what the
    // window that just closed held back.
    #[test]
    fn the_summary_names_the_closed_window() {
        let mut budget = DropBudget::new();
        admit_n(&mut budget, 10_000, LINES_PER_WINDOW + 5);
        let next = budget.admit(10_000 + u64::from(WINDOW_MS));
        assert_eq!(
            next.summary,
            Some(Suppressed {
                lines: 5,
                window_ms: WINDOW_MS
            })
        );
        assert!(next.emit, "and the drop that closed it is printed too");
    }

    // A window that suppressed nothing is silent. A summary per second on an
    // idle board would be the flood the limiter exists to prevent.
    #[test]
    fn a_window_under_the_limit_produces_no_summary() {
        let mut budget = DropBudget::new();
        admit_n(&mut budget, 10_000, LINES_PER_WINDOW);
        assert_eq!(budget.admit(20_000).summary, None);
    }

    // A storm that stops dead still reports: flush closes the expired window
    // without a drop to carry it.
    #[test]
    fn flush_reports_a_storm_that_stopped() {
        let mut budget = DropBudget::new();
        admit_n(&mut budget, 10_000, LINES_PER_WINDOW + 42);
        assert_eq!(budget.flush(10_500), None, "window still open, say nothing");
        assert_eq!(
            budget.flush(10_000 + u64::from(WINDOW_MS)),
            Some(Suppressed {
                lines: 42,
                window_ms: WINDOW_MS
            })
        );
        assert_eq!(
            budget.flush(99_999),
            None,
            "and it reports once, not on every later iteration"
        );
    }

    // Called every main-loop iteration on a board that has dropped nothing at
    // all, flush must stay silent and cheap.
    #[test]
    fn flush_on_an_untouched_budget_is_silent() {
        let mut budget = DropBudget::new();
        for tick in 0..1_000 {
            assert_eq!(budget.flush(tick * 37), None);
        }
    }

    // The boundary is inclusive: a drop exactly WINDOW_MS after the window
    // opened belongs to the next window. Off by one here would make the
    // reported rate wrong by a whole window under a steady storm.
    #[test]
    fn the_boundary_belongs_to_the_next_window() {
        let mut budget = DropBudget::new();
        admit_n(&mut budget, 1_000, LINES_PER_WINDOW + 1);
        let just_inside = budget.admit(1_000 + u64::from(WINDOW_MS) - 1);
        assert!(!just_inside.emit, "still the old window, still over budget");
        assert_eq!(just_inside.summary, None);
        let at_the_boundary = budget.admit(1_000 + u64::from(WINDOW_MS));
        assert!(at_the_boundary.emit);
        assert_eq!(
            at_the_boundary.summary,
            Some(Suppressed {
                lines: 2,
                window_ms: WINDOW_MS
            }),
            "the two suppressed inside the closed window"
        );
    }

    // A clock that steps backwards must not wrap the window arithmetic or
    // panic in a debug build; it just leaves the window where it was.
    #[test]
    fn a_backwards_clock_leaves_the_window_open() {
        let mut budget = DropBudget::new();
        admit_n(&mut budget, 50_000, LINES_PER_WINDOW + 1);
        assert_eq!(budget.flush(10), None);
        let still_old = budget.admit(10);
        assert!(!still_old.emit);
        assert_eq!(still_old.summary, None);
    }

    // The #421 budget table, pinned: the one claim the module docs make that
    // can rot silently is that a full window's worth of worst-case `[DROP]`
    // lines plus the periodic context a reader needs still fit the 8 KiB
    // LOG_RING over one 30 s capture window. Render the real worst-case line
    // through the crate that owns its shape rather than estimating bytes —
    // this is the test that forced LINES_PER_WINDOW from 3 to 2 when the
    // 16-hex `ph=` correlator joined the line.
    #[test]
    fn the_ring_derivation_still_holds() {
        // The debug CDC ring the derivation is sized against
        // (`leviculum-nrf/src/log.rs:25`, `LOG_RING_SIZE`).
        const LOG_RING_SIZE: usize = 8 * 1024;
        // The periodic lines a capture is taken for, per 30 s window:
        // `[STACK]` every 2 s (~110 B), `[FW_BUILD]` every 5 s (~70 B), and
        // the `[TRANSPORT]` block every 30 s (counter line plus per-interface
        // byte lines, ~3 × ~90 B).
        const CONTEXT_BUDGET: usize = 15 * 110 + 6 * 70 + 3 * 90;
        const CAPTURE_WINDOW_S: usize = 30;

        let reason = "overheard-transport-id";
        assert_eq!(
            reason.len(),
            leviculum_log_line::DROP_REASON_KEBAB_MAX,
            "widest kebab in the taxonomy, pinned on both ends"
        );
        let body = leviculum_log_line::PacketDroppedBody {
            reason,
            ph: Some([0xff; 8]),
            dest: [0xff; 16],
            iface_in: 9,
        };
        let mut buf = [0u8; 1024];
        // Eleven days of uptime; a longer soak buys one more digit per order
        // of magnitude, which the inequality's slack covers.
        let line = leviculum_log_line::format_line(
            &mut buf,
            "[DROP] ",
            format_args!("{body}"),
            999_999_999,
        );
        let worst = line.len();
        let windows = CAPTURE_WINDOW_S as u32 * 1_000 / WINDOW_MS;
        let drops = LINES_PER_WINDOW as usize * windows as usize * worst;
        assert!(
            drops + CONTEXT_BUDGET <= LOG_RING_SIZE,
            "a full 30 s storm ({drops} B of {worst} B [DROP] lines) plus \
             {CONTEXT_BUDGET} B of context no longer fits the {LOG_RING_SIZE} B \
             LOG_RING; lower LINES_PER_WINDOW or shrink the line"
        );
    }

    // The suppressed counter saturates rather than wrapping: a wrapped count
    // would report a storm as a trickle, which is worse than reporting a
    // ceiling.
    #[test]
    fn the_suppressed_counter_saturates() {
        let mut budget = DropBudget::new();
        budget.admit(0);
        budget.suppressed = u32::MAX;
        budget.admit(0);
        assert_eq!(budget.suppressed, u32::MAX);
    }
}
