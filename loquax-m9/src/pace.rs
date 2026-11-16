//! When the board draws (1507, move 4 of the 2026-11-07 structure
//! review's M9 list): at most once per frame of the panel.
//!
//! The ST7789 scans its memory out at its frame rate and says each
//! vertical blank on its TE line (Sitronix ST7789V datasheet, 9.1.37
//! TEON, 0x35, mode 0: the blank only). A draw between two blanks is all
//! the reader can see of that frame, so the board collects what changed
//! (the model's folds, the keys, the I/O core's pages) until the next
//! blank and draws once. Two keys inside one frame cost one draw, not two.
//!
//! The TE net is GPIO19 by the Meshtastic and MeshCore variants
//! (`docs/src/concepts/thinknode-m9.md`, "Display"); no schematic says it.
//! A line that never moves (not wired, or not this pin) must not stop the
//! screen: after [`SILENT_US`] without an edge the pace falls back to the
//! clock, one draw per [`PERIOD_US`], the panel's 60 Hz at its reset
//! frame rate (FRCTRL2 0x0F, datasheet 9.2.8), and says so
//! ([`Pace::source`]).

/// The panel's frame period at its reset frame rate: 60 Hz.
pub const PERIOD_US: u32 = 16_667;

/// A TE line silent this long is taken for dead: three frames.
pub const SILENT_US: u32 = 3 * PERIOD_US;

/// What paces the draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The panel's TE line.
    Te,
    /// The clock: the line has been silent for [`SILENT_US`].
    Clock,
}

impl Source {
    /// The word the board's `[DISP]` line says.
    pub fn name(self) -> &'static str {
        match self {
            Source::Te => "te",
            Source::Clock => "clock",
        }
    }
}

/// The draw pace: the TE edges counted so far against the count at the
/// last draw, and the clock for a line that stays silent.
///
/// Times are microseconds of a clock that wraps at `u32`, compared by
/// wrapping difference, so the board can hand its 64-bit uptime truncated.
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    /// The edge count the last draw saw.
    drawn_edges: u32,
    /// When the last draw was.
    drawn_at: Option<u32>,
    /// The last edge count seen and when it last moved.
    seen_edges: u32,
    moved_at: Option<u32>,
}

impl Default for Pace {
    fn default() -> Self {
        Self::new()
    }
}

/// `later - earlier` on a wrapping microsecond clock.
fn since(later: u32, earlier: u32) -> u32 {
    later.wrapping_sub(earlier)
}

impl Pace {
    /// A pace that has seen no edge and drawn nothing.
    pub const fn new() -> Self {
        Self {
            drawn_edges: 0,
            drawn_at: None,
            seen_edges: 0,
            moved_at: None,
        }
    }

    /// Note the edge count `edges` at `now`.
    fn see(&mut self, edges: u32, now: u32) {
        if self.moved_at.is_none() || edges != self.seen_edges {
            self.seen_edges = edges;
            self.moved_at = Some(now);
        }
    }

    /// What paces the draws at `now`, with `edges` counted so far.
    pub fn source(&mut self, edges: u32, now: u32) -> Source {
        self.see(edges, now);
        // A count that never moved since boot is a silent line too.
        let silent = self
            .moved_at
            .is_none_or(|at| since(now, at) >= SILENT_US || self.seen_edges == 0);
        match silent {
            true => Source::Clock,
            false => Source::Te,
        }
    }

    /// Whether a draw owed now may go to the panel: a new frame began
    /// since the last draw. The first draw is always due.
    pub fn due(&mut self, edges: u32, now: u32) -> bool {
        let Some(drawn_at) = self.drawn_at else {
            return true;
        };
        match self.source(edges, now) {
            Source::Te => edges != self.drawn_edges,
            Source::Clock => since(now, drawn_at) >= PERIOD_US,
        }
    }

    /// The board drew at `now` with `edges` counted.
    pub fn drew(&mut self, edges: u32, now: u32) {
        self.see(edges, now);
        self.drawn_edges = edges;
        self.drawn_at = Some(now);
    }

    /// How long until a draw owed now may go, at most: for the board's
    /// idle wait. Zero when it may go now.
    pub fn wait_us(&mut self, edges: u32, now: u32) -> u32 {
        if self.due(edges, now) {
            return 0;
        }
        match (self.source(edges, now), self.drawn_at) {
            (Source::Clock, Some(at)) => PERIOD_US.saturating_sub(since(now, at)),
            _ => PERIOD_US,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two changes inside one frame are one draw; the next edge lets the
    /// next one go.
    #[test]
    fn one_draw_per_te_frame() {
        let mut pace = Pace::new();
        assert!(pace.due(1, 0), "the first draw is always due");
        pace.drew(1, 0);
        assert!(!pace.due(1, 2_000), "the same frame");
        assert!(!pace.due(1, 9_000), "still the same frame");
        assert!(pace.due(2, 16_700), "the next blank");
        pace.drew(2, 16_700);
        assert!(!pace.due(2, 17_000));
        assert_eq!(pace.source(2, 17_000), Source::Te);
    }

    /// A line that never moves falls back to the clock's 60 Hz, so the
    /// screen never stops on a dead TE.
    #[test]
    fn a_silent_line_paces_by_the_clock() {
        let mut pace = Pace::new();
        assert!(pace.due(0, 0));
        pace.drew(0, 0);
        assert_eq!(pace.source(0, 1_000), Source::Clock, "no edge since boot");
        assert!(!pace.due(0, 10_000));
        assert!(pace.due(0, PERIOD_US));
        assert_eq!(pace.wait_us(0, 10_000), PERIOD_US - 10_000);
    }

    /// A line that stops after it ran is dead after three frames, and
    /// the clock takes over; when it runs again, TE paces again.
    #[test]
    fn a_line_that_stops_hands_over_to_the_clock_and_back() {
        let mut pace = Pace::new();
        pace.drew(5, 0);
        assert!(!pace.due(5, 20_000), "one frame silent: still TE");
        assert_eq!(pace.source(5, 20_000), Source::Te);
        assert_eq!(pace.source(5, SILENT_US + 1), Source::Clock);
        assert!(pace.due(5, SILENT_US + 1), "the clock lets it go");
        pace.drew(5, SILENT_US + 1);
        assert!(pace.due(6, SILENT_US + 2), "the line runs again: its edge");
        assert_eq!(pace.source(6, SILENT_US + 2), Source::Te);
    }

    /// The clock wraps at u32 microseconds (71 minutes) without a stall.
    #[test]
    fn the_clock_may_wrap() {
        let mut pace = Pace::new();
        let near = u32::MAX - 1_000;
        pace.drew(0, near);
        assert!(!pace.due(0, near.wrapping_add(5_000)));
        assert!(pace.due(0, near.wrapping_add(PERIOD_US)));
    }
}
