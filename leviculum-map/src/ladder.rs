//! The map's frame budget (1422, the eye candy study of 2026-11-04,
//! section 6): which passes a frame runs while the view moves, and in
//! which order a view at rest grows back to the full picture.
//!
//! One constant, [`TARGET_FPS`], sets the budget of a moving frame; no
//! platform has a table of its own. The frontend times each pass of a
//! frame (one timestamp a pass, never one a tile or a pixel) and hands
//! the times in; the [`Ladder`] keeps a moving average a pass and one of
//! the whole moving frame, and from them alone picks the rung a moving
//! frame runs at:
//!
//! - rung 0, full: every pass;
//! - rung 1, light: the passes of cost class C that only finish the
//!   picture drop out ([`Pass::lowest`]);
//! - rung 2, minimal: the ground, the pins, the tracks and the radar; the
//!   names ride along as the sprites they were, not placed anew, and the
//!   tiles drawn are drafts without their edges smoothed.
//!
//! The rules, as the study numbers them: the average of the moving frame
//! over the budget three frames running lowers the rung at once, even in
//! a gesture (3); the rung rises by one at most, and only as a gesture
//! starts, when the gesture before ended under 60 % of the budget (4); a
//! frame over three times the average is a spike, counted in the average
//! as twice it and never towards a descent (5); at rest the passes come
//! back one by one, the cheapest in orientation first, as many a frame
//! as their measured costs fit in one budget (6); and a pass with a
//! buffer of its own declares its bytes and is skipped, never the
//! picture, when the measured free memory cannot hold them (7).
//!
//! Pure: the time comes from outside, so the rules are tested with
//! invented times and no clock. `no_std`, so the M9 runs the same struct.

/// Frames a second a moving map aims at: the one constant.
pub const TARGET_FPS: u32 = 30;
/// A moving frame's budget in microseconds.
pub const BUDGET_US: u32 = 1_000_000 / TARGET_FPS;
/// The rungs: 0 full, 1 light, 2 minimal.
pub const RUNGS: u8 = 3;
/// Moving frames whose average is over the budget, one after another,
/// before the rung falls.
pub const DESCEND_AFTER: u8 = 3;
/// A gesture that ended under this share of the budget, in per cent,
/// lets the next one start a rung higher.
pub const ASCEND_UNDER_PERCENT: u32 = 60;
/// A frame over this many times the average is a spike ...
pub const SPIKE_TIMES: u32 = 3;
/// ... and counts in the average as this many times it.
pub const SPIKE_CAP_TIMES: u32 = 2;
/// The averages move by an eighth of each new time.
const EMA_SHIFT: u32 = 3;

/// The passes of a map frame, in the order a view at rest adds them:
/// what orients first (the ground, where one is, the others, their
/// tracks), the finish last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pass {
    /// The tiles composed, the missing ones drawn: the map itself.
    Ground,
    /// The radar's rings where no region covers the view (1386).
    Radar,
    /// The pins and their labels.
    Pins,
    /// The tracks (1385).
    Tracks,
    /// The building footprints' fill (1192), in the tiles.
    Buildings,
    /// The names placed anew by the rule (1225).
    Labels,
    /// The edges of the ground's fills and lines blended by their
    /// coverage (1422, the study's 2.1a): tiles drawn without it while
    /// the view moves are drafts a frame with it draws again.
    Smooth,
}

/// The passes.
pub const PASSES: usize = 7;

/// What a pass costs, host, 480 x 320, a frame or a tile (the study's
/// classes): A under 0.1 ms, B 0.1 to 1 ms, C over 1 ms or a factor on
/// the fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cost {
    A,
    B,
    C,
}

impl Pass {
    /// Every pass, in the order a view at rest adds them.
    pub const ALL: [Pass; PASSES] = [
        Pass::Ground,
        Pass::Radar,
        Pass::Pins,
        Pass::Tracks,
        Pass::Buildings,
        Pass::Labels,
        Pass::Smooth,
    ];

    const fn index(self) -> usize {
        match self {
            Pass::Ground => 0,
            Pass::Radar => 1,
            Pass::Pins => 2,
            Pass::Tracks => 3,
            Pass::Buildings => 4,
            Pass::Labels => 5,
            Pass::Smooth => 6,
        }
    }

    /// The deepest rung a moving frame still runs this pass at: what
    /// orients survives to the minimal rung, the names placed anew
    /// (class C on a phone) and the smoothed edges go below the light
    /// one.
    pub const fn lowest(self) -> u8 {
        match self {
            Pass::Ground | Pass::Radar | Pass::Pins | Pass::Tracks | Pass::Buildings => 2,
            Pass::Labels | Pass::Smooth => 1,
        }
    }

    /// Its cost class (the study's table, section 6).
    pub const fn class(self) -> Cost {
        match self {
            Pass::Radar | Pass::Pins => Cost::A,
            Pass::Tracks | Pass::Smooth => Cost::B,
            Pass::Ground | Pass::Buildings | Pass::Labels => Cost::C,
        }
    }

    /// Its name in a measuring tool's line.
    pub const fn name(self) -> &'static str {
        match self {
            Pass::Ground => "ground",
            Pass::Radar => "radar",
            Pass::Pins => "pins",
            Pass::Tracks => "tracks",
            Pass::Buildings => "buildings",
            Pass::Labels => "labels",
            Pass::Smooth => "smooth",
        }
    }
}

/// A set of passes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Layers(u16);

impl Layers {
    /// No pass.
    pub const NONE: Self = Self(0);
    /// Every pass.
    pub const ALL: Self = Self((1 << PASSES) - 1);

    /// Whether `pass` is in.
    pub const fn has(self, pass: Pass) -> bool {
        self.0 & (1 << pass.index()) != 0
    }

    /// The set with `pass`.
    pub const fn with(self, pass: Pass) -> Self {
        Self(self.0 | (1 << pass.index()))
    }

    /// The passes a moving frame runs at `rung`.
    pub fn of_rung(rung: u8) -> Self {
        Pass::ALL
            .iter()
            .filter(|p| p.lowest() >= rung)
            .fold(Self::NONE, |l, &p| l.with(p))
    }

    /// The passes of both sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The set without the passes of `other`.
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether every pass of `other` is in.
    pub const fn covers(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// What a view at rest does next ([`Ladder::rest_step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rest {
    /// Run this pass.
    Run(Pass),
    /// Skip it: its buffer does not fit the free memory.
    Skip(Pass),
    /// The picture is full.
    Full,
}

/// A moving average in microseconds, seeded by its first time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Average(Option<u32>);

impl Average {
    /// Take `us` in. A spike, over [`SPIKE_TIMES`] the average, counts as
    /// [`SPIKE_CAP_TIMES`] it; whether it was one.
    fn take(&mut self, us: u32) -> bool {
        let Some(avg) = self.0 else {
            self.0 = Some(us);
            return false;
        };
        let spike = us > avg.saturating_mul(SPIKE_TIMES);
        let t = match spike {
            true => avg.saturating_mul(SPIKE_CAP_TIMES),
            false => us,
        };
        let next = i64::from(avg) + (i64::from(t) - i64::from(avg)) / (1 << EMA_SHIFT);
        self.0 = Some(u32::try_from(next.max(0)).unwrap_or(u32::MAX));
        spike
    }
}

/// The planner: the averages, the rung, and where the last gesture
/// ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ladder {
    passes: [Average; PASSES],
    motion: Average,
    rung: u8,
    /// Moving frames over the budget, one after another.
    over: u8,
    moving: bool,
    /// The moving average the last gesture ended at.
    last_gesture: Option<u32>,
    /// A rung held whatever the times say: a measuring tool's.
    held: Option<u8>,
}

impl Ladder {
    /// A planner at the full rung, nothing measured.
    pub const fn new() -> Self {
        Self {
            passes: [Average(None); PASSES],
            motion: Average(None),
            rung: 0,
            over: 0,
            moving: false,
            last_gesture: None,
            held: None,
        }
    }

    /// The rung moving frames run at.
    pub fn rung(&self) -> u8 {
        self.held.unwrap_or(self.rung)
    }

    /// Hold moving frames at `rung`, or with `None` let the times pick
    /// it again: a measuring tool's fixed rung.
    pub fn hold(&mut self, rung: Option<u8>) {
        self.held = rung.map(|r| r.min(RUNGS - 1));
    }

    /// The rung held, if one is ([`hold`](Self::hold)).
    pub fn held(&self) -> Option<u8> {
        self.held
    }

    /// The passes a moving frame runs.
    pub fn motion_layers(&self) -> Layers {
        Layers::of_rung(self.rung())
    }

    /// A gesture is under way.
    pub fn moving(&self) -> bool {
        self.moving
    }

    /// The moving frame's average, in microseconds.
    pub fn motion_us(&self) -> Option<u32> {
        self.motion.0
    }

    /// The average of `pass`, in microseconds.
    pub fn pass_us(&self, pass: Pass) -> Option<u32> {
        self.passes[pass.index()].0
    }

    /// `pass` took `us` in a frame, moving or at rest.
    pub fn observe(&mut self, pass: Pass, us: u32) {
        self.passes[pass.index()].take(us);
    }

    /// A gesture starts (rule 4): one rung up when the last one ended
    /// under [`ASCEND_UNDER_PERCENT`] of the budget, never more.
    pub fn gesture_start(&mut self) {
        if self.moving {
            return;
        }
        self.moving = true;
        let calm = self.last_gesture.take().is_some_and(|us| {
            u64::from(us) * 100 < u64::from(BUDGET_US) * u64::from(ASCEND_UNDER_PERCENT)
        });
        if calm && self.rung > 0 {
            self.rung -= 1;
            self.motion = Average(None);
            self.over = 0;
        }
    }

    /// A moving frame took `us` (rules 3 and 5): three frames over the
    /// budget one after another lower the rung at once; a spike neither
    /// counts towards that nor breaks the run.
    pub fn motion_frame(&mut self, us: u32) {
        if !self.moving {
            self.gesture_start();
        }
        if self.motion.take(us) {
            return;
        }
        let over = self.motion.0.is_some_and(|avg| avg > BUDGET_US);
        self.over = match over {
            true => self.over.saturating_add(1),
            false => 0,
        };
        if self.over >= DESCEND_AFTER && self.rung + 1 < RUNGS {
            self.rung += 1;
            self.over = 0;
            // The average was the higher rung's.
            self.motion = Average(None);
        }
    }

    /// The gesture ended: the average it ended at is what the next one
    /// starts by.
    pub fn gesture_end(&mut self) {
        if !self.moving {
            return;
        }
        self.moving = false;
        self.last_gesture = self.motion.0;
    }

    /// What a view at rest that has run the passes of `done` does next
    /// (rules 6 and 7): the next pass in [`Pass::ALL`]'s order, or a skip
    /// when the bytes it declares (`bytes`) are more than `free`.
    pub fn rest_step(&self, done: Layers, free: usize, bytes: impl Fn(Pass) -> usize) -> Rest {
        match Pass::ALL.iter().find(|&&p| !done.has(p)) {
            None => Rest::Full,
            Some(&p) if bytes(p) > free => Rest::Skip(p),
            Some(&p) => Rest::Run(p),
        }
    }

    /// The passes a frame at rest runs next, added to `done`: one after
    /// another while their measured costs fit one budget together, at
    /// least one, a pass not measured yet counted as free; and the passes
    /// skipped for memory, which count as run.
    pub fn rest_plan(
        &self,
        done: Layers,
        free: usize,
        bytes: impl Fn(Pass) -> usize,
    ) -> (Layers, Layers) {
        let (mut run, mut skipped) = (done, Layers::NONE);
        let spent: u32 = Pass::ALL
            .iter()
            .filter(|&&p| done.has(p))
            .map(|&p| self.pass_us(p).unwrap_or(0))
            .fold(0, u32::saturating_add);
        let mut spent = spent;
        let mut added = false;
        loop {
            match self.rest_step(run, free, &bytes) {
                Rest::Full => break,
                Rest::Skip(p) => {
                    run = run.with(p);
                    skipped = skipped.with(p);
                }
                Rest::Run(p) => {
                    let cost = self.pass_us(p).unwrap_or(0);
                    if added && spent.saturating_add(cost) > BUDGET_US {
                        break;
                    }
                    spent = spent.saturating_add(cost);
                    run = run.with(p);
                    added = true;
                }
            }
        }
        (run, skipped)
    }

    /// Every pass at once, those whose bytes do not fit `free` skipped:
    /// a frame at rest under a held rung, a measuring tool's full frame.
    pub fn rest_all(&self, free: usize, bytes: impl Fn(Pass) -> usize) -> (Layers, Layers) {
        let (mut run, mut skipped) = (Layers::NONE, Layers::NONE);
        loop {
            match self.rest_step(run, free, &bytes) {
                Rest::Full => return (run, skipped),
                Rest::Skip(p) => {
                    run = run.with(p);
                    skipped = skipped.with(p);
                }
                Rest::Run(p) => run = run.with(p),
            }
        }
    }

    /// How many units of work of `unit_us` each fit one budget, at least
    /// one: a pass split into tiles so no input waits longer than a
    /// budget (rule 6).
    pub fn units_in_budget(&self, unit_us: u32) -> usize {
        (BUDGET_US / unit_us.max(1)).max(1) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOW: u32 = BUDGET_US * 2;
    const FAST: u32 = BUDGET_US / 4;

    #[test]
    fn three_frames_over_the_budget_lower_the_rung_in_the_gesture() {
        let mut l = Ladder::new();
        l.gesture_start();
        l.motion_frame(SLOW);
        l.motion_frame(SLOW);
        assert_eq!(l.rung(), 0, "two frames over are not three");
        l.motion_frame(SLOW);
        assert_eq!(l.rung(), 1, "the third lowers it at once");
        for _ in 0..3 {
            l.motion_frame(SLOW);
        }
        assert_eq!(l.rung(), 2);
        for _ in 0..9 {
            l.motion_frame(SLOW);
        }
        assert_eq!(l.rung(), RUNGS - 1, "never below the minimal rung");
    }

    #[test]
    fn a_frame_under_the_budget_breaks_the_run() {
        let mut l = Ladder::new();
        l.motion_frame(BUDGET_US + 1);
        l.motion_frame(BUDGET_US + 1);
        // The average falls under the budget again.
        for _ in 0..8 {
            l.motion_frame(FAST);
        }
        l.motion_frame(BUDGET_US + 1);
        assert_eq!(l.rung(), 0);
    }

    #[test]
    fn the_rung_rises_only_as_a_gesture_starts_and_by_one() {
        let mut l = Ladder::new();
        for _ in 0..6 {
            l.motion_frame(SLOW);
        }
        assert_eq!(l.rung(), 2);
        // Cheap frames in the same gesture never raise it.
        for _ in 0..20 {
            l.motion_frame(FAST);
        }
        assert_eq!(l.rung(), 2, "no ascent in a gesture");
        l.gesture_end();
        l.gesture_start();
        assert_eq!(l.rung(), 1, "one up as the next gesture starts");
        l.gesture_start();
        assert_eq!(l.rung(), 1, "at most one up a gesture");
        l.gesture_end();
        // That gesture ran no frame: nothing to go up by.
        l.gesture_start();
        assert_eq!(l.rung(), 1);
    }

    #[test]
    fn a_gesture_over_sixty_per_cent_of_the_budget_keeps_the_rung() {
        let mut l = Ladder::new();
        for _ in 0..3 {
            l.motion_frame(SLOW);
        }
        assert_eq!(l.rung(), 1);
        for _ in 0..30 {
            l.motion_frame(BUDGET_US * 61 / 100);
        }
        l.gesture_end();
        l.gesture_start();
        assert_eq!(l.rung(), 1, "61 % is not under 60 %");
        for _ in 0..30 {
            l.motion_frame(BUDGET_US * 59 / 100 - 1);
        }
        l.gesture_end();
        l.gesture_start();
        assert_eq!(l.rung(), 0);
    }

    #[test]
    fn a_spike_never_lowers_the_rung_and_is_capped() {
        let mut l = Ladder::new();
        l.gesture_start();
        for _ in 0..4 {
            l.motion_frame(FAST);
        }
        let before = l.motion_us().unwrap_or(0);
        // A 200 ms stall, again and again: each a spike against the
        // average it meets.
        l.motion_frame(200_000);
        assert_eq!(l.rung(), 0);
        let after = l.motion_us().unwrap_or(0);
        assert_eq!(after, before + before / 8, "counted as twice the average");
        for _ in 0..2 {
            l.motion_frame(200_000);
        }
        assert_eq!(l.rung(), 0, "spikes are no sustained load");
    }

    #[test]
    fn a_sustained_load_lowers_it_though() {
        let mut l = Ladder::new();
        for _ in 0..4 {
            l.motion_frame(BUDGET_US * 9 / 10);
        }
        // Under three times the average each, so no spike: the average
        // climbs over the budget and stays.
        for _ in 0..30 {
            l.motion_frame(BUDGET_US * 2);
        }
        assert!(l.rung() >= 1);
    }

    #[test]
    fn at_rest_the_passes_come_back_in_order_within_a_budget() {
        let mut l = Ladder::new();
        l.observe(Pass::Ground, BUDGET_US / 2);
        l.observe(Pass::Radar, 10);
        l.observe(Pass::Pins, 10);
        l.observe(Pass::Tracks, BUDGET_US / 4);
        l.observe(Pass::Buildings, BUDGET_US / 2);
        l.observe(Pass::Labels, BUDGET_US);
        let none = |_| 0;
        let (first, skipped) = l.rest_plan(Layers::NONE, usize::MAX, none);
        assert_eq!(skipped, Layers::NONE);
        let want = Layers::NONE
            .with(Pass::Ground)
            .with(Pass::Radar)
            .with(Pass::Pins)
            .with(Pass::Tracks);
        assert_eq!(first, want, "what fits one budget, in order");
        let (second, _) = l.rest_plan(first, usize::MAX, none);
        assert_eq!(
            second,
            first.with(Pass::Buildings),
            "at least one more a frame"
        );
        let (third, _) = l.rest_plan(second, usize::MAX, none);
        assert_eq!(third, second.with(Pass::Labels));
        let (fourth, _) = l.rest_plan(third, usize::MAX, none);
        assert_eq!(fourth, Layers::ALL, "the smoothing, not measured yet");
        assert_eq!(l.rest_step(fourth, usize::MAX, none), Rest::Full);
    }

    #[test]
    fn a_pass_whose_buffer_does_not_fit_is_skipped_not_the_picture() {
        let l = Ladder::new();
        let bytes = |p| match p {
            Pass::Labels => 4 << 20,
            _ => 0,
        };
        let done = Layers::of_rung(2);
        assert_eq!(l.rest_step(done, 1 << 20, bytes), Rest::Skip(Pass::Labels));
        assert_eq!(l.rest_step(done, 8 << 20, bytes), Rest::Run(Pass::Labels));
        let (run, skipped) = l.rest_plan(Layers::NONE, 1 << 20, bytes);
        assert_eq!(run, Layers::ALL, "the rest runs, nothing measured");
        assert_eq!(skipped, Layers::NONE.with(Pass::Labels));
    }

    #[test]
    fn the_rungs_keep_what_orients() {
        for rung in 0..RUNGS {
            let l = Layers::of_rung(rung);
            for p in [Pass::Ground, Pass::Pins, Pass::Tracks, Pass::Radar] {
                assert!(l.has(p), "{} at rung {rung}", p.name());
            }
        }
        for p in [Pass::Labels, Pass::Smooth] {
            assert!(Layers::of_rung(1).has(p));
            assert!(!Layers::of_rung(2).has(p));
        }
        assert_eq!(Layers::of_rung(0), Layers::ALL);
    }

    #[test]
    fn a_held_rung_wins_and_lets_go() {
        let mut l = Ladder::new();
        l.hold(Some(7));
        assert_eq!(l.rung(), RUNGS - 1);
        assert_eq!(l.motion_layers(), Layers::of_rung(2));
        l.hold(None);
        assert_eq!(l.rung(), 0);
        assert_eq!(
            Layers::ALL.without(Layers::of_rung(2)),
            Layers::NONE.with(Pass::Labels).with(Pass::Smooth)
        );
        let bytes = |p| usize::from(p == Pass::Labels) << 30;
        assert_eq!(
            l.rest_all(1 << 20, bytes),
            (Layers::ALL, Layers::NONE.with(Pass::Labels))
        );
    }

    #[test]
    fn a_long_pass_is_split_into_units_of_a_budget() {
        let l = Ladder::new();
        assert_eq!(l.units_in_budget(BUDGET_US * 3), 1);
        assert_eq!(l.units_in_budget(BUDGET_US / 4), 4);
        assert_eq!(l.units_in_budget(0), BUDGET_US as usize);
    }
}
