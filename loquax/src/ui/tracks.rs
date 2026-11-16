//! A peer's track on the map (`docs/src/concepts/loquax-tracks.md`, 1385):
//! the current movement drawn as lines, everything older collapsed into
//! places. The map answers "where is she now, is she moving, how did she
//! get there"; a year of a tracker in a handbag is one large blob at home
//! and a few small ones, never a scribble.
//!
//! Every rule here is a pure function over a peer's fixes in fix-time
//! order ([`TrackFix`], the compact copy of a [`Fix`] the model keeps per
//! peer), the time window the reader chose ([`TrackWindow`]) and the
//! clock; the drawing is the frontends' ([`crate::screenmap`] for a pixel
//! screen, [`cells`] for the terminal). So the thresholds are tested
//! here, without a map.
//!
//! The rules, in the page's order: [`segments`] (rule 1, the gap, the
//! speed outlier, the accuracy drop), [`dwells`] (rule 2), the episode
//! ([`episode_start`], rule 3), [`places`] (rule 4), the age steps
//! ([`Age`], rule 5), the hue ([`hue`], rule 6), the wedge ([`wedge`],
//! rule 7). Rule 8's quiet line is gone from the screen (1447, walk 1
//! finding 7): the pin's own age says how long a peer has been silent, in
//! the catalogue's words, and a second age beside it ("no fix for 1 d"
//! over "vor 28 Std.") was one position with three ages. [`track`] puts
//! the rules together under a fixed drawing budget per peer.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_map::mercator::{project, LatLon, World};

use crate::telemetry::Fix;
use crate::ui::strings::{Lang, StrId};
use crate::when::WallTime;

/// Two consecutive fixes further apart in time than this are not joined
/// (rule 1).
pub const JOIN_GAP_SECS: i64 = 30 * 60;
/// Two consecutive fixes whose implied speed is above this, in metres per
/// second times 100, are not joined: 250 km/h (rule 1).
pub const MAX_SPEED_E2: i64 = 6_944;
/// A fix whose accuracy is worse than this, in metres times 100, is never
/// on the line nor in a dwell (rule 1); 0 says the sender did not know,
/// which passes.
pub const MAX_ACCURACY_E2: u16 = 50_000;
/// Fixes within the larger of their accuracy and this many metres of a
/// dwell's first fix belong to the dwell (rule 2).
pub const DWELL_RADIUS_M: f64 = 50.0;
/// A dwell lasts at least this long (rule 2).
pub const DWELL_SECS: i64 = 10 * 60;
/// A dwell this long or longer ends an episode (rules 2 and 3).
pub const EPISODE_DWELL_SECS: i64 = 3 * 3600;
/// The current episode reaches back at least this far (rule 3).
pub const EPISODE_FLOOR_SECS: i64 = 24 * 3600;
/// A place credits a fix with the time to the next fix in the same place,
/// at most this much: a tracker that was off for a week at home did not
/// stay a week more at home (rule 4).
pub const PLACE_CREDIT_SECS: i64 = 6 * 3600;
/// A place's cell, in screen pixels at the render zoom (rule 4).
pub const PLACE_CELL_PX: f64 = 8.0;
/// A place's cell joins a larger one closer than this many cells, so a
/// place the grid cuts is one blob (rule 4).
pub const PLACE_JOIN_CELLS: f64 = 2.0;
/// Places of one peer, the largest kept (rule 4).
pub const PLACE_BUDGET: usize = 48;
/// Places that carry a label: the largest few (rule 4).
pub const PLACE_LABELS: usize = 3;
/// Segments of one peer drawn at most (rule 4's budget).
pub const SEGMENT_BUDGET: usize = 2_000;
/// Dwell blobs of one peer drawn at most, the newest kept.
pub const DWELL_BUDGET: usize = 64;
/// Outlier dots of one peer drawn at most, the newest kept.
pub const DOT_BUDGET: usize = 64;
/// Two fixes on the line closer than this on screen are drawn as one: a
/// pixel's worth of line costs one segment, however many fixes it holds.
pub const DECIMATE_PX: f64 = 1.0;
/// The newest fix carries a wedge above this speed, metres per second
/// times 100 (rule 7).
pub const MOVING_E2: i64 = 150;
/// A dwell blob's radius in logical pixels, at most (rule 2).
pub const DWELL_RADIUS_MAX_PX: u8 = 14;
/// A place blob's radius in logical pixels, at most (rule 4).
pub const PLACE_RADIUS_MAX_PX: u8 = 20;
/// The hues a peer's track takes (rule 6).
pub const HUES: u8 = 8;

/// One fix as the track keeps it: what the rules read of a [`Fix`],
/// sixteen bytes and a time instead of the Telemeter it came in, so a year
/// of a tracker's fixes is a few megabytes in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackFix {
    /// [`Fix::order_time`]: the claim, clamped to the receive time where
    /// it is ahead of our clock.
    pub t: i64,
    pub lat_e6: i32,
    pub lon_e6: i32,
    /// Metres per second times 100, 0 when unknown.
    pub speed_e2: u32,
    /// Degrees times 100.
    pub bearing_e2: i32,
    /// Metres times 100, 0 when unknown.
    pub accuracy_e2: u16,
    /// It arrived late ([`Fix::late_secs`], 1386): the segment it ends is
    /// dashed, the peer was out of reach meanwhile. In the padding, so a
    /// fix stays 32 bytes.
    pub late: bool,
}

impl TrackFix {
    pub fn of(fix: &Fix) -> Self {
        let l = &fix.location;
        Self {
            t: fix.order_time(),
            lat_e6: l.latitude_e6,
            lon_e6: l.longitude_e6,
            speed_e2: l.speed_e2,
            bearing_e2: l.bearing_e2,
            accuracy_e2: l.accuracy_e2,
            late: fix.late_secs().is_some(),
        }
    }

    /// Where it lands on the projection.
    pub fn at(&self) -> World {
        project(LatLon::from_e6(self.lat_e6, self.lon_e6))
    }

    /// Whether its accuracy keeps it off the line and out of a dwell.
    pub fn vague(&self) -> bool {
        self.accuracy_e2 > MAX_ACCURACY_E2
    }

    fn accuracy_m(&self) -> f64 {
        f64::from(self.accuracy_e2) / 100.0
    }
}

/// Insert `fix` into `fixes`, kept in time order, one fix a time: a fix
/// heard twice (in a message and announced) is kept once.
pub fn insert(fixes: &mut Vec<TrackFix>, fix: TrackFix) {
    match fixes.binary_search_by_key(&fix.t, |f| f.t) {
        Ok(_) => {}
        Err(i) => fixes.insert(i, fix),
    }
}

/// Metres between two fixes, on the sphere's tangent plane at their mean
/// latitude: exact to a fraction of a percent at the distances a track
/// joins, and cheap enough for a year of fixes.
pub fn metres(a: &TrackFix, b: &TrackFix) -> f64 {
    const M_PER_E6: f64 = 0.111_194_93;
    let mean = (f64::from(a.lat_e6) + f64::from(b.lat_e6)) / 2.0 * 1e-6;
    let dy = f64::from(b.lat_e6) - f64::from(a.lat_e6);
    let dx = (f64::from(b.lon_e6) - f64::from(a.lon_e6)) * libm::cos(mean.to_radians());
    libm::sqrt(dx * dx + dy * dy) * M_PER_E6
}

/// The speed between two fixes, metres per second times 100; two fixes
/// at one second count a second apart.
pub fn implied_speed_e2(a: &TrackFix, b: &TrackFix) -> i64 {
    let dt = (b.t - a.t).abs().max(1) as f64;
    (metres(a, b) * 100.0 / dt) as i64
}

/// Whether two consecutive fixes are joined by a line (rule 1): at most
/// [`JOIN_GAP_SECS`] apart and no faster than [`MAX_SPEED_E2`].
pub fn joined(a: &TrackFix, b: &TrackFix) -> bool {
    (b.t - a.t).abs() <= JOIN_GAP_SECS && implied_speed_e2(a, b) <= MAX_SPEED_E2
}

/// Which fixes are outliers (rule 1): one that fails the speed rule
/// against both neighbours, a spike out and back. The first and the last
/// fix have one neighbour and are no outlier: a real jump is a gap, and
/// the newest fix is where the peer is. Vague fixes are skipped, neither
/// outlier nor neighbour.
pub fn outliers(fixes: &[TrackFix]) -> Vec<bool> {
    let mut out = alloc::vec![false; fixes.len()];
    let clear: Vec<usize> = (0..fixes.len()).filter(|&i| !fixes[i].vague()).collect();
    for w in clear.windows(3) {
        let (a, b, c) = (&fixes[w[0]], &fixes[w[1]], &fixes[w[2]]);
        let fast = |p: &TrackFix, q: &TrackFix| implied_speed_e2(p, q) > MAX_SPEED_E2;
        if fast(a, b) && fast(b, c) {
            out[w[1]] = true;
        }
    }
    out
}

/// A stay (rule 2): fixes within the larger of their accuracy and
/// [`DWELL_RADIUS_M`] of the first, at least [`DWELL_SECS`] apart from
/// first to last. Indices into the fixes it was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dwell {
    pub first: usize,
    pub last: usize,
    pub start: i64,
    pub end: i64,
    /// The mean of its fixes.
    pub at: World,
}

impl Dwell {
    pub fn secs(&self) -> i64 {
        self.end - self.start
    }

    /// Whether it ends an episode (rule 3).
    pub fn ends_episode(&self) -> bool {
        self.secs() >= EPISODE_DWELL_SECS
    }
}

/// The dwells of `fixes` (rule 2), oldest first. Outliers and vague
/// fixes neither join nor break one. There is no gap limit inside a dwell:
/// a board that reports hourly from home stays at home between reports.
pub fn dwells(fixes: &[TrackFix], outlier: &[bool]) -> Vec<Dwell> {
    let usable = |i: usize| !fixes[i].vague() && !outlier.get(i).copied().unwrap_or(false);
    let mut out = Vec::new();
    let mut i = 0;
    while i < fixes.len() {
        if !usable(i) {
            i += 1;
            continue;
        }
        let anchor = &fixes[i];
        let (mut last, mut j) = (i, i + 1);
        let (mut sx, mut sy, mut n) = (0u64, 0u64, 0u64);
        let at = anchor.at();
        sx += u64::from(at.x);
        sy += u64::from(at.y);
        n += 1;
        while j < fixes.len() {
            if usable(j) {
                let f = &fixes[j];
                let reach = DWELL_RADIUS_M.max(anchor.accuracy_m()).max(f.accuracy_m());
                if metres(anchor, f) > reach {
                    break;
                }
                let at = f.at();
                sx += u64::from(at.x);
                sy += u64::from(at.y);
                n += 1;
                last = j;
            }
            j += 1;
        }
        let (start, end) = (fixes[i].t, fixes[last].t);
        if end - start >= DWELL_SECS {
            out.push(Dwell {
                first: i,
                last,
                start,
                end,
                at: World {
                    x: (sx / n) as u32,
                    y: (sy / n) as u32,
                },
            });
            i = last + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Where the current episode starts (rule 3): after the last dwell of
/// [`EPISODE_DWELL_SECS`] or more, or at the first fix after a silence that
/// long (a peer that shares only while it moves stopped somewhere we did
/// not see), and at least [`EPISODE_FLOOR_SECS`] before `now`. Without
/// either the episode is everything.
pub fn episode_start(fixes: &[TrackFix], dwells: &[Dwell], now: i64) -> i64 {
    let floor = now - EPISODE_FLOOR_SECS;
    let stay = dwells
        .iter()
        .rev()
        .find(|d| d.ends_episode())
        .map(|d| d.end);
    let silence = fixes
        .windows(2)
        .rev()
        .find(|w| w[1].t - w[0].t >= EPISODE_DWELL_SECS)
        .map(|w| w[1].t);
    match stay.max(silence) {
        Some(start) => start.min(floor),
        None => i64::MIN,
    }
}

/// The time window the map draws lines in: a map control, not a setting,
/// but a hidden one (1419): the map's menu on the phone and in the window
/// opens a single choice of [`TrackWindow::CHOICES`], `w` steps through
/// them in the terminal. The default is the current episode; any other
/// puts its word after the map's title ([`TrackWindow::title`]), so the
/// reader knows why older lines show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrackWindow {
    /// The current episode (rule 3).
    #[default]
    Episode,
    /// Since local midnight.
    Today,
    /// The local day before.
    Yesterday,
    /// Since Monday 00:00 local time.
    Week,
    /// Since the first of the month, 00:00 local time.
    Month,
    /// Every fix.
    All,
}

impl TrackWindow {
    /// The five named windows, in the menu's order.
    pub const NAMED: [TrackWindow; 5] = [
        TrackWindow::Today,
        TrackWindow::Yesterday,
        TrackWindow::Week,
        TrackWindow::Month,
        TrackWindow::All,
    ];

    /// The menu's single choice: the default first, then the named five.
    pub const CHOICES: [TrackWindow; 6] = [
        TrackWindow::Episode,
        TrackWindow::Today,
        TrackWindow::Yesterday,
        TrackWindow::Week,
        TrackWindow::Month,
        TrackWindow::All,
    ];

    /// The window's entry in the menu.
    pub fn label(self) -> StrId {
        match self {
            TrackWindow::Episode => StrId::TracksNow,
            TrackWindow::Today => StrId::TracksToday,
            TrackWindow::Yesterday => StrId::TracksYesterday,
            TrackWindow::Week => StrId::TracksWeek,
            TrackWindow::Month => StrId::TracksMonth,
            TrackWindow::All => StrId::TracksAll,
        }
    }

    /// The word after the map's title: none for the default, which needs
    /// no reason, else the window's entry.
    pub fn title_word(self) -> Option<StrId> {
        match self {
            TrackWindow::Episode => None,
            other => Some(other.label()),
        }
    }

    /// The map screen's title in `lang`: "Map" with the default, "Map ·
    /// This week" with another window.
    pub fn title(self, lang: Lang) -> String {
        let map = StrId::Map.text(lang);
        match self.title_word() {
            None => String::from(map),
            Some(word) => format!("{map} · {}", word.text(lang)),
        }
    }

    /// The control's word for it, which the phone's chips send back.
    pub fn word(self) -> &'static str {
        match self {
            TrackWindow::Episode => "now",
            TrackWindow::Today => "today",
            TrackWindow::Yesterday => "yesterday",
            TrackWindow::Week => "week",
            TrackWindow::Month => "month",
            TrackWindow::All => "all",
        }
    }

    /// The window a control's word names.
    pub fn of_word(word: &str) -> Option<Self> {
        Self::CHOICES.into_iter().find(|w| w.word() == word)
    }

    /// The next window `w` steps to in the terminal: now, today, ... all,
    /// now again; `forward` false goes the other way.
    pub fn step(self, forward: bool) -> Self {
        let all = Self::CHOICES;
        let i = all.iter().position(|w| *w == self).unwrap_or(0);
        let n = all.len();
        all[match forward {
            true => (i + 1) % n,
            false => (i + n - 1) % n,
        }]
    }

    /// The window's span `[from, to)` in unix seconds at `now`, the
    /// episode's from the fixes and their dwells.
    pub fn bounds(self, fixes: &[TrackFix], dwells: &[Dwell], now: &WallTime) -> (i64, i64) {
        let c = now.civil();
        let midnight = now.unix_secs
            - i64::from(c.hour) * 3600
            - i64::from(c.minute) * 60
            - i64::from(c.second);
        match self {
            TrackWindow::Episode => (episode_start(fixes, dwells, now.unix_secs), i64::MAX),
            TrackWindow::Today => (midnight, i64::MAX),
            TrackWindow::Yesterday => (midnight - 86_400, midnight),
            TrackWindow::Week => (midnight - i64::from(c.weekday) * 86_400, i64::MAX),
            TrackWindow::Month => (
                WallTime::from_civil(c.year, c.month, 1, 0, 0, now.offset_secs).unix_secs,
                i64::MAX,
            ),
            TrackWindow::All => (i64::MIN, i64::MAX),
        }
    }
}

/// How old a part of the track is, in steps and not a gradient (rule 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Age {
    /// Under 10 min: full.
    Fresh,
    /// To 1 h: strong.
    Strong,
    /// To 6 h: half.
    Half,
    /// To 24 h: faint.
    Faint,
    /// Older: very faint, never invisible.
    Old,
}

impl Age {
    /// The step a fix `secs` old is in.
    pub fn of(secs: i64) -> Self {
        match secs {
            s if s < 10 * 60 => Age::Fresh,
            s if s < 3600 => Age::Strong,
            s if s < 6 * 3600 => Age::Half,
            s if s < 24 * 3600 => Age::Faint,
            _ => Age::Old,
        }
    }

    /// Its opacity on a pixel screen; the floor is 25 %.
    pub fn alpha(self) -> f32 {
        match self {
            Age::Fresh => 1.0,
            Age::Strong => 0.8,
            Age::Half => 0.5,
            Age::Faint => 0.35,
            Age::Old => 0.25,
        }
    }

    /// The terminal's four levels, 3 the strongest: full and strong share
    /// the top one.
    pub fn level(self) -> u8 {
        match self {
            Age::Fresh | Age::Strong => 3,
            Age::Half => 2,
            Age::Faint => 1,
            Age::Old => 0,
        }
    }
}

/// A peer's hue, one of [`HUES`], from its address: every device that
/// sees the peer draws it in the same hue (rule 6). `None` for our own
/// track, which is neutral grey.
pub fn hue(peer: &[u8; 16], own: bool) -> Option<u8> {
    (!own).then_some(peer[0] % HUES)
}

/// The eight hues on a light map (rule 6): blue, orange, green, red,
/// violet, teal, magenta, brown; apart in hue and in lightness, so two stay
/// apart at a quarter of their opacity.
pub const HUE_LIGHT: [[u8; 3]; 8] = [
    [0x1f, 0x5f, 0xd0],
    [0xe0, 0x70, 0x00],
    [0x12, 0x8f, 0x4a],
    [0xd0, 0x22, 0x2e],
    [0x7a, 0x3f, 0xc8],
    [0x00, 0x8c, 0x96],
    [0xc0, 0x2a, 0x90],
    [0x8a, 0x5a, 0x1e],
];
/// The same hues on the dark map, lighter, so they hold against its
/// ground.
pub const HUE_DARK: [[u8; 3]; 8] = [
    [0x6a, 0xa4, 0xff],
    [0xff, 0xa8, 0x40],
    [0x4c, 0xd0, 0x80],
    [0xff, 0x6a, 0x6a],
    [0xb8, 0x8c, 0xff],
    [0x3c, 0xd4, 0xd8],
    [0xff, 0x7a, 0xd0],
    [0xd8, 0xa8, 0x68],
];
/// Our own track: neutral grey on either map.
pub const OWN_LIGHT: [u8; 3] = [0x60, 0x60, 0x60];
pub const OWN_DARK: [u8; 3] = [0xb0, 0xb0, 0xb0];

/// The colour of a track of `hue` ([`hue`]) on a light or a dark map.
pub fn colour(hue: Option<u8>, dark: bool) -> [u8; 3] {
    match (hue, dark) {
        (Some(h), false) => HUE_LIGHT[usize::from(h % HUES)],
        (Some(h), true) => HUE_DARK[usize::from(h % HUES)],
        (None, false) => OWN_LIGHT,
        (None, true) => OWN_DARK,
    }
}

/// The ANSI colour of a hue for a terminal of sixteen: blue, yellow,
/// green, red, magenta, cyan, bright magenta, bright yellow; ours white
/// or black by the map.
pub fn ansi16(hue: Option<u8>, dark: bool) -> u8 {
    match hue {
        Some(h) => [4, 3, 2, 1, 5, 6, 13, 11][usize::from(h % HUES)],
        None if dark => 7,
        None => 8,
    }
}

/// A blob's radius in logical pixels for a stay of `secs` (rules 2 and
/// 4): 4 px at ten minutes and 2 px more for every doubling, at most
/// `max`.
pub fn blob_radius(secs: i64, max: u8) -> u8 {
    let tens = (secs.max(DWELL_SECS) / DWELL_SECS) as u64;
    let doublings = 63 - tens.leading_zeros() as u64;
    (4 + 2 * doublings).min(u64::from(max)) as u8
}

/// A line between two fixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub from: World,
    pub to: World,
    /// The newer end's age.
    pub age: Age,
    /// The newer end arrived late (1386): drawn dashed.
    pub late: bool,
}

/// A fix drawn as a faint dot and never on the line: an outlier or a
/// vague fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dot {
    pub at: World,
}

/// A dwell inside the window, drawn as a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blob {
    pub at: World,
    pub radius: u8,
    /// Its end's age.
    pub age: Age,
}

/// A place outside the window: a faint blob sized by the total dwell, the
/// largest few labelled with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub at: World,
    pub radius: u8,
    pub dwell_secs: i64,
    pub label: Option<String>,
}

/// The wedge on the newest fix's pin (rule 7): where it is, and the
/// bearing in degrees clockwise from north.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wedge {
    pub at: World,
    pub bearing: i32,
}

/// A peer's track as the map draws it, under the drawing budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub peer: [u8; 16],
    /// [`hue`]; `None` for our own, grey.
    pub hue: Option<u8>,
    pub segments: Vec<Segment>,
    pub dots: Vec<Dot>,
    pub dwells: Vec<Blob>,
    pub places: Vec<Place>,
    pub wedge: Option<Wedge>,
    /// The newest fix, where the pin is.
    pub head: Option<World>,
}

/// The lines inside `[from, to)` (rule 1): consecutive fixes on the line
/// joined where [`joined`], none inside one dwell (its blob stands for
/// it), outliers and vague fixes off it. Fixes closer than `unit` world
/// units to the last one kept are dropped first ([`DECIMATE_PX`]), and
/// what is left over [`SEGMENT_BUDGET`] is thinned evenly, so a year in
/// the window costs no more than the budget.
pub fn segments(
    fixes: &[TrackFix],
    outlier: &[bool],
    dwells: &[Dwell],
    (from, to): (i64, i64),
    now: i64,
    unit: f64,
) -> Vec<Segment> {
    let in_dwell = dwell_of(fixes.len(), dwells);
    let near = (DECIMATE_PX * unit).max(0.0);
    let on_line = |i: usize, f: &TrackFix| f.t >= from && f.t < to && !f.vague() && !outlier[i];
    // The newest fix on the line is always kept: the line reaches the pin.
    let newest = fixes
        .iter()
        .enumerate()
        .rev()
        .find(|(i, f)| on_line(*i, f))
        .map(|(i, _)| i);
    let mut out = Vec::new();
    // The last fix kept, and the last fix on the line: two fixes are
    // joined along the chain of their neighbours, so a pixel's worth of
    // fixes is one segment however long it took to walk.
    let mut kept: Option<(usize, World)> = None;
    let mut prev: Option<usize> = None;
    for (i, f) in fixes.iter().enumerate() {
        if !on_line(i, f) {
            continue;
        }
        let at = f.at();
        let linked = prev.is_some_and(|p| joined(&fixes[p], f));
        prev = Some(i);
        match kept {
            Some((j, start)) if linked => {
                let close = world_distance(start, at) < near;
                if close && Some(i) != newest {
                    continue;
                }
                let same_dwell = in_dwell[j].is_some() && in_dwell[j] == in_dwell[i];
                if !same_dwell {
                    out.push(Segment {
                        from: start,
                        to: at,
                        age: Age::of(now - f.t),
                        late: f.late,
                    });
                }
                kept = Some((i, at));
            }
            _ => kept = Some((i, at)),
        }
    }
    thin(out, SEGMENT_BUDGET)
}

/// Which dwell each fix is in.
fn dwell_of(n: usize, dwells: &[Dwell]) -> Vec<Option<usize>> {
    let mut out = alloc::vec![None; n];
    for (k, d) in dwells.iter().enumerate() {
        for slot in out.iter_mut().take(d.last + 1).skip(d.first) {
            *slot = Some(k);
        }
    }
    out
}

/// Distance on the projection in world units.
fn world_distance(a: World, b: World) -> f64 {
    let dx = f64::from(a.x.wrapping_sub(b.x) as i32);
    let dy = f64::from(b.y) - f64::from(a.y);
    libm::sqrt(dx * dx + dy * dy)
}

/// At most `budget` of `items`, evenly spaced, the newest always kept.
fn thin<T>(items: Vec<T>, budget: usize) -> Vec<T> {
    let n = items.len();
    if n <= budget || budget == 0 {
        return items;
    }
    items
        .into_iter()
        .enumerate()
        .filter(|(i, _)| i * budget / n != (i + 1) * budget / n)
        .map(|(_, t)| t)
        .collect()
}

/// Every fix outside `[from, to)` and not `skip`ped (an outlier, or a fix
/// of a dwell drawn as a blob inside the window) collected into places
/// (rule 4): cells of
/// `cell` world units, each crediting its fixes with the time to the next
/// fix when that fix is in the same cell, at most [`PLACE_CREDIT_SECS`]. A
/// cell with less than [`DWELL_SECS`] is passage and drawn as nothing; of
/// the others the largest [`PLACE_BUDGET`] are places, the largest
/// [`PLACE_LABELS`] labelled with their total.
pub fn places(
    fixes: &[TrackFix],
    skip: &[bool],
    (from, to): (i64, i64),
    cell: f64,
    lang: Lang,
) -> Vec<Place> {
    let cell = cell.max(1.0);
    let key = |w: World| {
        (
            (f64::from(w.x) / cell) as i64,
            (f64::from(w.y) / cell) as i64,
        )
    };
    // Per cell: the sum of x and y, the fixes, the dwell.
    let mut cells: BTreeMap<(i64, i64), (u64, u64, u64, i64)> = BTreeMap::new();
    let mut last: Option<((i64, i64), i64)> = None;
    for (i, f) in fixes.iter().enumerate() {
        let outside = f.t < from || f.t >= to;
        if !outside || f.vague() || skip[i] {
            last = None;
            continue;
        }
        let at = f.at();
        let k = key(at);
        let entry = cells.entry(k).or_insert((0, 0, 0, 0));
        entry.0 += u64::from(at.x);
        entry.1 += u64::from(at.y);
        entry.2 += 1;
        if let Some((lk, lt)) = last {
            if lk == k {
                entry.3 += (f.t - lt).clamp(0, PLACE_CREDIT_SECS);
            }
        }
        last = Some((k, f.t));
    }
    // The cells with a stay, the largest first; a cell beside a larger
    // one joins it, so a place cut by the grid is one blob.
    let mut kept: Vec<(u64, u64, u64, i64)> =
        cells.into_values().filter(|c| c.3 >= DWELL_SECS).collect();
    kept.sort_by_key(|c| core::cmp::Reverse(c.3));
    let mut merged: Vec<(u64, u64, u64, i64)> = Vec::new();
    for c in kept {
        let at = |c: &(u64, u64, u64, i64)| World {
            x: (c.0 / c.2) as u32,
            y: (c.1 / c.2) as u32,
        };
        let here = at(&c);
        match merged
            .iter_mut()
            .find(|m| world_distance(at(m), here) < cell * PLACE_JOIN_CELLS)
        {
            Some(m) => *m = (m.0 + c.0, m.1 + c.1, m.2 + c.2, m.3 + c.3),
            None => merged.push(c),
        }
    }
    let mut out: Vec<Place> = merged
        .into_iter()
        .map(|(sx, sy, n, dwell)| Place {
            at: World {
                x: (sx / n) as u32,
                y: (sy / n) as u32,
            },
            radius: blob_radius(dwell, PLACE_RADIUS_MAX_PX),
            dwell_secs: dwell,
            label: None,
        })
        .collect();
    out.sort_by_key(|p| (core::cmp::Reverse(p.dwell_secs), p.at));
    out.truncate(PLACE_BUDGET);
    for p in out.iter_mut().take(PLACE_LABELS) {
        p.label = Some(dwell_words(p.dwell_secs, lang));
    }
    out
}

/// The wedge on the newest fix (rule 7): when it moves faster than
/// [`MOVING_E2`], by its own speed or, where it carries none, by the
/// speed from the fix before it; along its own bearing, or along the last
/// two fixes where it carries no motion.
pub fn wedge(fixes: &[TrackFix], outlier: &[bool]) -> Option<Wedge> {
    let usable: Vec<usize> = (0..fixes.len())
        .filter(|&i| !fixes[i].vague() && !outlier[i])
        .collect();
    let newest = &fixes[*usable.last()?];
    let before = usable
        .len()
        .checked_sub(2)
        .map(|k| &fixes[usable[k]])
        .filter(|b| joined(b, newest));
    let reported = newest.speed_e2 > 0;
    let speed = match (reported, before) {
        (true, _) => i64::from(newest.speed_e2),
        (false, Some(b)) => implied_speed_e2(b, newest),
        (false, None) => 0,
    };
    if speed <= MOVING_E2 {
        return None;
    }
    let bearing = match (reported, before) {
        (true, _) => newest.bearing_e2 / 100,
        (false, Some(b)) => bearing_of(b, newest),
        (false, None) => return None,
    };
    Some(Wedge {
        at: newest.at(),
        bearing: bearing.rem_euclid(360),
    })
}

/// The bearing from `a` to `b` in whole degrees clockwise from north.
pub fn bearing_of(a: &TrackFix, b: &TrackFix) -> i32 {
    let mean = (f64::from(a.lat_e6) + f64::from(b.lat_e6)) / 2.0 * 1e-6;
    let dy = f64::from(b.lat_e6) - f64::from(a.lat_e6);
    let dx = (f64::from(b.lon_e6) - f64::from(a.lon_e6)) * libm::cos(mean.to_radians());
    let deg = libm::atan2(dx, dy).to_degrees();
    (libm::round(deg) as i32).rem_euclid(360)
}

/// How long a place was stayed at, in the catalogue's words (1447):
/// `45 min`, `3 h`, `12 d`; never the English of [`crate::telemetry::span`]
/// on a German screen.
pub fn dwell_words(secs: i64, lang: Lang) -> String {
    use crate::ui::strings::{Arg, Text};
    let secs = secs.max(0);
    let (id, n) = match secs {
        ..3600 => (StrId::MinutesAgo, (secs / 60).max(1)),
        3600..86_400 => (StrId::HoursAgo, secs / 3600),
        _ => (StrId::DaysAgo, secs / 86_400),
    };
    Text::Key(id, Vec::from([Arg::Int(n)])).resolve(lang)
}

/// The world units a place's cell spans at a view whose screen pixel is
/// `unit` world units, [`PLACE_CELL_PX`] logical pixels at `scale`.
pub fn place_cell(unit: f64, scale: f32) -> f64 {
    unit * PLACE_CELL_PX * f64::from(scale.max(0.1))
}

/// `peer`'s track as the map draws it in `window` at `now`, on a view
/// whose screen pixel spans `unit` world units, drawn at `scale`; `fixes`
/// in time order ([`insert`]), the places' words in `lang`.
#[allow(clippy::too_many_arguments)]
pub fn track(
    peer: [u8; 16],
    own: bool,
    fixes: &[TrackFix],
    window: TrackWindow,
    now: &WallTime,
    unit: f64,
    scale: f32,
    lang: Lang,
) -> Track {
    let outlier = outliers(fixes);
    let stays = dwells(fixes, &outlier);
    let bounds = window.bounds(fixes, &stays, now);
    let (from, to) = bounds;
    let t = now.unix_secs;
    let segments = segments(fixes, &outlier, &stays, bounds, t, unit);
    let inside = |time: i64| time >= from && time < to;
    let dots: Vec<Dot> = fixes
        .iter()
        .enumerate()
        .filter(|(i, f)| inside(f.t) && (f.vague() || outlier[*i]))
        .map(|(_, f)| Dot { at: f.at() })
        .collect();
    let dots = newest(dots, DOT_BUDGET);
    let blobs: Vec<Blob> = stays
        .iter()
        .filter(|d| inside(d.end))
        .map(|d| Blob {
            at: d.at,
            radius: blob_radius(d.secs(), DWELL_RADIUS_MAX_PX),
            age: Age::of(t - d.end),
        })
        .collect();
    let dwells = newest(blobs, DWELL_BUDGET);
    let mut skip = outlier.clone();
    for d in stays.iter().filter(|d| inside(d.end)) {
        for s in skip.iter_mut().take(d.last + 1).skip(d.first) {
            *s = true;
        }
    }
    let cell = place_cell(unit, scale);
    let mut places = places(fixes, &skip, bounds, cell, lang);
    // A place under the pin carries no words (1447): the pin's age is the
    // one label there, never a stay's length laid over it.
    let head = fixes.last().map(TrackFix::at);
    if let Some(head) = head {
        for p in places
            .iter_mut()
            .filter(|p| world_distance(p.at, head) < cell * PLACE_JOIN_CELLS)
        {
            p.label = None;
        }
    }
    Track {
        peer,
        hue: hue(&peer, own),
        segments,
        dots,
        dwells,
        places,
        wedge: wedge(fixes, &outlier),
        head,
    }
}

/// Every peer's fixes the map draws a track of: the peer, whether it is
/// us, the fixes in time order.
pub type TrackInput = Vec<([u8; 16], bool, alloc::sync::Arc<Vec<TrackFix>>)>;

/// The tracks of `input` ([`track`] for each), the places' words in
/// `lang`.
pub fn tracks(
    input: &TrackInput,
    window: TrackWindow,
    now: &WallTime,
    unit: f64,
    scale: f32,
    lang: Lang,
) -> Vec<Track> {
    input
        .iter()
        .map(|(peer, own, fixes)| track(*peer, *own, fixes, window, now, unit, scale, lang))
        .collect()
}

/// The peers of `input` whose newest fix lies inside `window` at `now`
/// (1386): what "fit all" brings into view. The current episode is the
/// last [`EPISODE_FLOOR_SECS`] for this: a peer silent longer has ended
/// its episode by the silence (rule 3), though no fix after it says so.
pub fn inside(input: &TrackInput, window: TrackWindow, now: &WallTime) -> Vec<[u8; 16]> {
    input
        .iter()
        .filter(|(_, _, fixes)| {
            let Some(newest) = fixes.last() else {
                return false;
            };
            let (from, to) = match window {
                TrackWindow::Episode => (now.unix_secs - EPISODE_FLOOR_SECS, i64::MAX),
                _ => window.bounds(fixes, &[], now),
            };
            newest.t >= from && newest.t < to
        })
        .map(|(peer, _, _)| *peer)
        .collect()
}

/// The last `budget` of `items`.
fn newest<T>(mut items: Vec<T>, budget: usize) -> Vec<T> {
    let n = items.len();
    if n > budget {
        items.drain(..n - budget);
    }
    items
}

/// What a track costs to draw: segments, and blobs of every kind.
pub fn cost(track: &Track) -> (usize, usize) {
    (
        track.segments.len(),
        track.dots.len() + track.dwells.len() + track.places.len(),
    )
}

/// The most one track costs: [`SEGMENT_BUDGET`] segments, and the dots,
/// dwells and places together.
pub const BLOB_BUDGET: usize = DOT_BUDGET + DWELL_BUDGET + PLACE_BUDGET;

pub mod cells;
#[cfg(any(test, feature = "std"))]
pub mod scenes;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A fix `t` seconds in, `north` and `east` metres from a point in
    /// Florence.
    fn at(t: i64, north: f64, east: f64) -> TrackFix {
        const LAT: f64 = 43.7731;
        let lat = LAT + north / 111_194.93;
        let lon = 11.256 + east / (111_194.93 * libm::cos(LAT.to_radians()));
        TrackFix {
            t,
            lat_e6: (lat * 1e6) as i32,
            lon_e6: (lon * 1e6) as i32,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 500,
            late: false,
        }
    }

    /// Unix seconds of 2026-11-04 12:00 CET.
    fn noon() -> WallTime {
        WallTime::from_civil(2026, 11, 4, 12, 0, 3600)
    }

    fn all() -> (i64, i64) {
        (i64::MIN, i64::MAX)
    }

    #[test]
    fn metres_on_the_plane_match_the_offsets() {
        let d = metres(&at(0, 0.0, 0.0), &at(0, 300.0, 400.0));
        assert!((d - 500.0).abs() < 1.0, "{d}");
    }

    /// Rule 1, the gap: 30 min joins, a minute more does not.
    #[test]
    fn a_gap_over_half_an_hour_is_no_line() {
        let fixes = [at(0, 0.0, 0.0), at(1800, 0.0, 500.0), at(3661, 0.0, 1000.0)];
        let outlier = outliers(&fixes);
        let got = segments(&fixes, &outlier, &[], all(), 4000, 0.0);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].to, fixes[1].at());
    }

    /// Rule 1, the speed: 250 km/h joins, faster is a gap.
    #[test]
    fn a_jump_faster_than_250_kmh_is_no_line() {
        // 69 m/s joins; 70 m/s does not.
        let slow = [at(0, 0.0, 0.0), at(100, 0.0, 6_900.0)];
        let fast = [at(0, 0.0, 0.0), at(100, 0.0, 7_000.0)];
        for (fixes, n) in [(&slow, 1), (&fast, 0)] {
            let o = outliers(fixes);
            assert_eq!(segments(fixes, &o, &[], all(), 200, 0.0).len(), n);
        }
    }

    /// Rule 1, the outlier: a spike out and back is a dot, the line goes
    /// past it; a real jump at the end is a gap, not an outlier.
    #[test]
    fn a_spike_is_a_dot_and_the_line_goes_past_it() {
        let fixes = [
            at(0, 0.0, 0.0),
            at(60, 0.0, 100.0),
            at(120, 30_000.0, 100.0),
            at(180, 0.0, 200.0),
            at(240, 0.0, 300.0),
        ];
        let o = outliers(&fixes);
        assert_eq!(o, vec![false, false, true, false, false]);
        let got = segments(&fixes, &o, &[], all(), 300, 0.0);
        assert_eq!(got.len(), 3);
        assert_eq!((got[1].from, got[1].to), (fixes[1].at(), fixes[3].at()));
        let end = [
            at(0, 0.0, 0.0),
            at(60, 0.0, 100.0),
            at(120, 30_000.0, 100.0),
        ];
        assert_eq!(outliers(&end), vec![false; 3]);
        let t = track(
            [1; 16],
            false,
            &fixes,
            TrackWindow::All,
            &noon(),
            0.0,
            1.0,
            Lang::En,
        );
        assert_eq!(t.dots, vec![Dot { at: fixes[2].at() }]);
    }

    /// Rule 1, the accuracy: a fix vaguer than 500 m is off the line and
    /// a dot; 500 m itself is on it.
    #[test]
    fn a_vague_fix_is_off_the_line() {
        let mut fixes = [at(0, 0.0, 0.0), at(60, 0.0, 100.0), at(120, 0.0, 200.0)];
        fixes[1].accuracy_e2 = 50_001;
        let o = outliers(&fixes);
        let got = segments(&fixes, &o, &[], all(), 200, 0.0);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].from, got[0].to), (fixes[0].at(), fixes[2].at()));
        fixes[1].accuracy_e2 = 50_000;
        assert_eq!(segments(&fixes, &o, &[], all(), 200, 0.0).len(), 2);
    }

    /// Rule 2: ten minutes within 50 m is a dwell, nine are not; a fix
    /// with a wide accuracy widens the reach.
    #[test]
    fn ten_minutes_in_one_spot_is_a_dwell() {
        let stay: Vec<TrackFix> = (0..=10)
            .map(|m| at(m * 60, 20.0, 30.0 * (m % 2) as f64))
            .collect();
        let mut fixes = vec![at(-600, -2_000.0, 0.0)];
        fixes.extend(stay);
        fixes.push(at(1260, 2_000.0, 0.0));
        let o = outliers(&fixes);
        let d = dwells(&fixes, &o);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0].first, d[0].last, d[0].secs()), (1, 11, 600));
        let short: Vec<TrackFix> = (0..10).map(|m| at(m * 60, 0.0, 0.0)).collect();
        assert!(dwells(&short, &outliers(&short)).is_empty());
        // 80 m apart but each fix known to 100 m: one dwell.
        let mut wide = [at(0, 0.0, 0.0), at(700, 80.0, 0.0)];
        wide[1].accuracy_e2 = 10_000;
        assert_eq!(dwells(&wide, &outliers(&wide)).len(), 1);
    }

    /// Rule 2's radius: 4 px at ten minutes, 2 more a doubling, capped.
    #[test]
    fn a_blob_grows_with_its_dwell() {
        let r = |m: i64| blob_radius(m * 60, DWELL_RADIUS_MAX_PX);
        assert_eq!(
            [r(10), r(20), r(40), r(80), r(160), r(320), r(10_000)],
            [4, 6, 8, 10, 12, 14, 14]
        );
        assert_eq!(blob_radius(365 * 86_400, PLACE_RADIUS_MAX_PX), 20);
    }

    /// Rule 3: the episode starts after the last stay of 3 h, widened to
    /// 24 h; without one it is everything.
    #[test]
    fn the_episode_starts_after_the_last_long_stay() {
        let now = 100 * 3600;
        let stay = |start: i64, hours: i64| Dwell {
            first: 0,
            last: 0,
            start,
            end: start + hours * 3600,
            at: World { x: 0, y: 0 },
        };
        // A night at home ending 30 h ago, then a two-hour stop: the
        // episode starts at the night's end.
        let d = [stay(now - 40 * 3600, 10), stay(now - 10 * 3600, 2)];
        assert_eq!(episode_start(&[], &d, now), now - 30 * 3600);
        // The night ended 5 h ago: widened to 24 h.
        let d = [stay(now - 15 * 3600, 10)];
        assert_eq!(episode_start(&[], &d, now), now - 24 * 3600);
        assert_eq!(episode_start(&[], &[stay(now - 3600, 2)], now), i64::MIN);
        // A silence of 3 h ends one as a stay does; 2 h 59 does not.
        let quiet = [at(now - 40 * 3600, 0.0, 0.0), at(now - 37 * 3600, 0.0, 0.0)];
        assert_eq!(episode_start(&quiet, &[], now), now - 37 * 3600);
        let short = [
            at(now - 40 * 3600, 0.0, 0.0),
            at(now - 40 * 3600 + 10_799, 0.0, 0.0),
        ];
        assert_eq!(episode_start(&short, &[], now), i64::MIN);
    }

    /// Rule 3 on the map: lines only after the night, the night a blob,
    /// the walk before it neither line nor place (passage).
    #[test]
    fn lines_only_inside_the_episode() {
        let now = noon();
        let t0 = now.unix_secs - 50 * 3600;
        let mut fixes = Vec::new();
        // A walk two days ago, a night at a hotel, the walk on from it.
        for m in 0..30 {
            fixes.push(at(t0 + m * 60, 0.0, m as f64 * 50.0));
        }
        for h in 0..12 {
            fixes.push(at(t0 + 3600 + h * 3600, 0.0, 1_500.0));
        }
        let back = t0 + 13 * 3600;
        for m in 0..30 {
            fixes.push(at(back + m * 60, 100.0 + m as f64 * 50.0, 1_500.0));
        }
        let t = track(
            [1; 16],
            false,
            &fixes,
            TrackWindow::Episode,
            &now,
            64.0,
            1.0,
            Lang::En,
        );
        assert_eq!(t.segments.len(), 29, "the walk on, not the walk before");
        assert_eq!(t.dwells.len(), 1, "the night ends at the episode's start");
        assert!(t.places.is_empty(), "{:?}", t.places);
        let t = track(
            [1; 16],
            false,
            &fixes,
            TrackWindow::All,
            &now,
            64.0,
            1.0,
            Lang::En,
        );
        assert_eq!(t.segments.len(), 29 + 29, "the hotel is a blob, not a line");
        assert_eq!(t.dwells.len(), 1);
    }

    /// Rule 3, the 24 h widening: a stop of 4 h at noon today still shows
    /// this morning's way to it.
    #[test]
    fn the_episode_is_at_least_a_day() {
        let now = noon();
        let n = now.unix_secs;
        let mut fixes = Vec::new();
        for m in 0..60 {
            fixes.push(at(n - 6 * 3600 + m * 60, 0.0, m as f64 * 40.0));
        }
        for m in 0..=20 {
            fixes.push(at(n - 5 * 3600 + m * 15 * 60, 0.0, 2_400.0));
        }
        let t = track(
            [1; 16],
            false,
            &fixes,
            TrackWindow::Episode,
            &now,
            1.0,
            1.0,
            Lang::En,
        );
        assert!(t.segments.len() >= 59, "{}", t.segments.len());
        assert_eq!(t.dwells.len(), 1);
    }

    /// The named windows at noon on a Wednesday, CET.
    #[test]
    fn the_named_windows_are_local_days() {
        let now = noon();
        let midnight = WallTime::from_civil(2026, 11, 4, 0, 0, 3600).unix_secs;
        assert_eq!(
            TrackWindow::Today.bounds(&[], &[], &now),
            (midnight, i64::MAX)
        );
        assert_eq!(
            TrackWindow::Yesterday.bounds(&[], &[], &now),
            (midnight - 86_400, midnight)
        );
        let monday = WallTime::from_civil(2026, 11, 2, 0, 0, 3600).unix_secs;
        assert_eq!(TrackWindow::Week.bounds(&[], &[], &now).0, monday);
        let first = WallTime::from_civil(2026, 11, 1, 0, 0, 3600).unix_secs;
        assert_eq!(TrackWindow::Month.bounds(&[], &[], &now).0, first);
        assert_eq!(TrackWindow::Episode.step(true), TrackWindow::Today);
        assert_eq!(TrackWindow::Episode.step(false), TrackWindow::All);
        assert_eq!(TrackWindow::of_word("month"), Some(TrackWindow::Month));
        assert_eq!(TrackWindow::of_word("now"), Some(TrackWindow::Episode));
    }

    /// The menu (1419): the default first and marked by nothing in the
    /// title; any other window puts its word after the map's title.
    #[test]
    fn the_title_names_a_window_that_is_not_the_default() {
        use crate::ui::strings::Lang;
        assert_eq!(TrackWindow::CHOICES[0], TrackWindow::default());
        assert_eq!(&TrackWindow::CHOICES[1..], &TrackWindow::NAMED[..]);
        assert_eq!(TrackWindow::Episode.title_word(), None);
        assert_eq!(TrackWindow::Episode.title(Lang::De), "Karte");
        assert_eq!(TrackWindow::Episode.title(Lang::En), "Map");
        assert_eq!(TrackWindow::Week.title(Lang::De), "Karte · Diese Woche");
        assert_eq!(TrackWindow::Week.title(Lang::En), "Map · This week");
        assert_eq!(TrackWindow::All.title(Lang::De), "Karte · Alles");
        for w in TrackWindow::NAMED {
            assert_eq!(w.title_word(), Some(w.label()));
        }
    }

    /// Rule 4: a year of a tracker in a handbag, a fix every 15 min, is a
    /// large place at home, a smaller one at work, a few small ones; the
    /// commute between them is no place; the largest three are labelled.
    #[test]
    fn a_year_in_a_handbag_is_a_few_places() {
        let fixes = handbag_year(noon().unix_secs);
        let o = outliers(&fixes);
        // z14 at one logical pixel: 2^10 world units a pixel.
        let got = places(
            &fixes,
            &o,
            (i64::MAX, i64::MAX),
            place_cell(1024.0, 1.0),
            Lang::En,
        );
        assert!(got.len() <= 6, "{}", got.len());
        assert_eq!(got[0].radius, PLACE_RADIUS_MAX_PX);
        assert!(got[0].dwell_secs > got[1].dwell_secs);
        let labelled: Vec<_> = got.iter().filter_map(|p| p.label.clone()).collect();
        assert_eq!(labelled.len(), PLACE_LABELS.min(got.len()));
        assert!(labelled[0].ends_with(" d"), "{labelled:?}");
    }

    /// A year of a handbag tracker ending at `end`: home at night, work by
    /// day, a café on Saturdays, the commute between, every 15 min.
    pub(crate) fn handbag_year(end: i64) -> Vec<TrackFix> {
        let start = end - 365 * 86_400;
        let mut out = Vec::new();
        let mut t = start;
        while t < end {
            let hour = (t.rem_euclid(86_400)) / 3600;
            let day = (t - start) / 86_400;
            let weekend = day % 7 >= 5;
            let jitter = ((t / 900) % 7) as f64 * 5.0;
            let fix = match (weekend, hour) {
                (true, 10..=13) if day % 7 == 5 => at(t, 1_200.0 + jitter, -800.0),
                (false, 9..=16) => at(t, 3_000.0 + jitter, 2_500.0),
                (false, 8) | (false, 17) => {
                    let k = (t.rem_euclid(3600)) as f64 / 3600.0;
                    let k = if hour == 8 { k } else { 1.0 - k };
                    at(t, 3_000.0 * k, 2_500.0 * k)
                }
                _ => at(t, jitter, jitter),
            };
            out.push(fix);
            t += 900;
        }
        out
    }

    /// Rule 4's budget: 100 000 fixes of one peer in any window cost no
    /// more than the budget.
    #[test]
    fn a_hundred_thousand_fixes_stay_inside_the_budget() {
        let now = noon();
        let mut fixes = Vec::new();
        let start = now.unix_secs - 100_000 * 300;
        for i in 0..100_000i64 {
            // A wandering walk: always moving, never a dwell, so every fix
            // wants a line.
            let k = i as f64;
            fixes.push(at(
                start + i * 300,
                libm::sin(k / 300.0) * 20_000.0,
                k * 3.0 % 40_000.0,
            ));
        }
        assert_eq!(fixes.len(), 100_000);
        for window in [TrackWindow::Episode, TrackWindow::Month, TrackWindow::All] {
            for unit in [0.0, 64.0, 4096.0] {
                let t = track([7; 16], false, &fixes, window, &now, unit, 1.0, Lang::En);
                let (segments, blobs) = cost(&t);
                assert!(segments <= SEGMENT_BUDGET, "{window:?} {unit}: {segments}");
                assert!(blobs <= BLOB_BUDGET, "{window:?} {unit}: {blobs}");
            }
        }
    }

    #[test]
    fn thin_keeps_the_budget_and_the_newest() {
        let v: Vec<usize> = (0..10_000).collect();
        let t = thin(v, 2_000);
        assert_eq!(t.len(), 2_000);
        assert_eq!(t.last(), Some(&9_999));
    }

    /// Rule 5: the steps and the floor.
    #[test]
    fn age_goes_in_steps_with_a_floor() {
        let steps: Vec<Age> = [
            0, 599, 600, 3599, 3600, 21_599, 21_600, 86_399, 86_400, 9_999_999,
        ]
        .iter()
        .map(|s| Age::of(*s))
        .collect();
        use Age::*;
        assert_eq!(
            steps,
            [Fresh, Fresh, Strong, Strong, Half, Half, Faint, Faint, Old, Old]
        );
        assert_eq!(Old.alpha(), 0.25);
        let levels: Vec<u8> = [Fresh, Strong, Half, Faint, Old]
            .iter()
            .map(|a| a.level())
            .collect();
        assert_eq!(levels, [3, 3, 2, 1, 0]);
    }

    /// Rule 6: the hue is the address's, stable and one of eight; ours
    /// is grey.
    #[test]
    fn the_hue_comes_from_the_address() {
        let mut peer = [0x5a; 16];
        assert_eq!(hue(&peer, false), Some(0x5a % 8));
        peer[1] = 0;
        assert_eq!(hue(&peer, false), Some(2), "only the first byte");
        assert_eq!(hue(&peer, true), None);
        let seen: alloc::collections::BTreeSet<u8> =
            (0..=255u8).filter_map(|b| hue(&[b; 16], false)).collect();
        assert_eq!(seen.len(), 8);
    }

    /// Rule 7: a wedge above 1.5 m/s, along the field's bearing, or along
    /// the last two fixes when the field says nothing.
    #[test]
    fn the_wedge_points_where_it_goes() {
        let mut walking = [at(0, 0.0, 0.0), at(60, 0.0, 120.0)];
        let o = outliers(&walking);
        let w = wedge(&walking, &o).expect("2 m/s east");
        assert_eq!(w.bearing, 90);
        walking[1].speed_e2 = 300;
        walking[1].bearing_e2 = 18_000;
        assert_eq!(wedge(&walking, &o).map(|w| w.bearing), Some(180));
        let standing = [at(0, 0.0, 0.0), at(60, 0.0, 60.0)];
        assert_eq!(wedge(&standing, &outliers(&standing)), None, "1 m/s");
        let alone = [at(0, 0.0, 0.0)];
        assert_eq!(wedge(&alone, &outliers(&alone)), None);
    }

    /// 1447: a place's stay is said in the reader's language, and a
    /// place under the pin carries no words beside the pin's age.
    #[test]
    fn a_place_speaks_the_readers_language_and_none_under_the_pin() {
        assert_eq!(dwell_words(45 * 60, Lang::En), "45 min");
        assert_eq!(dwell_words(45 * 60, Lang::De), "45 Min.");
        assert_eq!(dwell_words(3 * 3600, Lang::De), "3 Std.");
        assert_eq!(dwell_words(2 * 86_400, Lang::De), "2 T.");
        let now = noon();
        let fixes = handbag_year(now.unix_secs);
        let unit = 1024.0;
        let t = track(
            [7; 16],
            false,
            &fixes,
            TrackWindow::Episode,
            &now,
            unit,
            1.0,
            Lang::De,
        );
        let head = t.head.expect("a head");
        let cell = place_cell(unit, 1.0);
        assert!(t.places.iter().any(|p| p.label.is_some()), "{:?}", t.places);
        for p in &t.places {
            if world_distance(p.at, head) < cell * PLACE_JOIN_CELLS {
                assert_eq!(p.label, None, "a place under the pin is quiet");
            }
            if let Some(words) = &p.label {
                assert!(!words.contains(" d") && !words.contains("fix"), "{words}");
            }
        }
    }

    #[test]
    fn insert_keeps_order_and_one_fix_a_time() {
        let mut v = Vec::new();
        for t in [5, 1, 3, 3, 9] {
            insert(&mut v, at(t, 0.0, 0.0));
        }
        let ts: Vec<i64> = v.iter().map(|f| f.t).collect();
        assert_eq!(ts, [1, 3, 5, 9]);
    }
}
