//! The radar (1386, `docs/src/concepts/loquax-tracks.md`, "Radar without
//! a map"): where the view has no region data, the map draws the peers
//! relative to the own position on a blank ground, north up, with rings
//! of distance about it, and each pin says how far and which way it is,
//! "1.3 km NE, 12 min ago". The tracks are drawn over it unchanged; only
//! the ground differs.
//!
//! Everything here is a pure function of the view, the regions and the
//! pins, so the terminal ([`cells`]), the window and the phone
//! ([`crate::screenmap::draw_radar`]) draw one radar. Without an own
//! position the radar centres on the newest peer and says so
//! ([`Radar::note`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_map::mercator::{unproject, World};
use leviculum_map::region::Header;
use leviculum_map::viewport::Viewport;
use leviculum_ui::Rect;

use super::map::MapPin;
use super::strings::{Arg, Lang, StrId, Text};
use super::tracks::cells::TrackCell;

/// Metres around the equator, the projection's 2^32 units.
const EQUATOR_M: f64 = 40_075_016.686;
/// The deepest zoom the reader may go to where no region is (1386): the
/// outline ends at [`leviculum_map::outline::MAX_ZOOM`], the radar goes
/// on to rings of 20 m.
pub const DEEPEST: u8 = 17;
/// The deepest zoom a fit goes to where no region holds its centre: a
/// lone pin's rings of 200 m, a group's extent as deep as it fits.
pub const FIT_ZOOM: u8 = 15;

/// `zooms` (a painter's: the regions' and the outline's) reaching at
/// least to [`DEEPEST`], where the radar draws.
pub fn zooms((shallowest, deepest): (u8, u8)) -> (u8, u8) {
    (shallowest, deepest.max(DEEPEST))
}

/// The most rings drawn, however small the step.
pub const MAX_RINGS: u32 = 12;
/// The rings a view's shorter half holds about: the step is the largest
/// round distance at most half of it ([`ring_step`]), so two or three
/// rings show around the centre at any zoom.
pub const RINGS_IN_HALF: f64 = 2.0;

/// The radar of a view: where its rings centre, and on whom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Radar {
    /// The own position, or the newest peer's without one.
    pub centre: World,
    /// The name of the peer it centres on when there is no own position;
    /// `None` when it centres on our own.
    pub on: Option<String>,
    /// The rings' step in metres, one of 1, 2 and 5 times a power of ten.
    pub step_m: u32,
    /// The rings' radii in metres, the step's multiples.
    pub rings: Vec<u32>,
    /// The language its note is said in (1447).
    pub lang: Lang,
}

impl Radar {
    /// The line over the radar, from the catalogue (1447, walk 1 finding
    /// 19): that no map is loaded for the area and the rings' step, and
    /// whom the rings are around without an own position. "radar" and
    /// "north up" were words for the map's maker, not its reader.
    pub fn note(&self) -> String {
        let step = Arg::Str(distance(f64::from(self.step_m)));
        let text = match &self.on {
            None => Text::Key(StrId::RadarNote, Vec::from([step])),
            Some(name) => Text::Key(
                StrId::RadarNoteOn,
                Vec::from([step, Arg::Str(name.clone())]),
            ),
        };
        text.resolve(self.lang)
    }
}

/// Whether `view` draws as a radar: deeper than the world outline goes
/// ([`leviculum_map::outline::MAX_ZOOM`]), where no region of `regions`
/// touches it. A view a region touches keeps its map, however little of
/// it is covered; a view zoomed out to the outline keeps the coasts.
pub fn shows(view: &Viewport, regions: &[Header]) -> bool {
    view.zoom() > leviculum_map::outline::MAX_ZOOM && !any_region(regions, view)
}

/// Whether a region of `regions` touches `view` at some zoom: without one
/// the map shows only the world's outline there, "No map here yet" on the
/// phone (1200). A box across the antimeridian counts as touching.
pub fn any_region(regions: &[Header], view: &Viewport) -> bool {
    let b = view.bounds();
    regions.iter().any(|h| {
        let r = h.bounds;
        r.west > r.east
            || b.west > b.east
            || (r.west <= b.east && b.west <= r.east && r.south <= b.north && b.south <= r.north)
    })
}

/// Metres one world unit spans at `at`'s latitude.
pub fn metres_per_unit(at: World) -> f64 {
    let lat = f64::from(unproject(at).lat_e7) * 1e-7;
    EQUATOR_M * libm::cos(lat.to_radians()) / 4_294_967_296.0
}

/// The largest of 1, 2 and 5 times a power of ten metres at most `metres`,
/// and 1 m at least.
pub fn ring_step(metres: f64) -> u32 {
    let mut step = 1u32;
    for power in 0..8 {
        let base = 10u32.pow(power);
        for k in [1, 2, 5] {
            let s = base * k;
            if f64::from(s) <= metres {
                step = s;
            }
        }
    }
    step
}

/// Metres between two points of the projection, on the sphere's tangent
/// plane at their mean latitude.
pub fn metres(a: World, b: World) -> f64 {
    const M_PER_E7: f64 = 0.011_119_493;
    let (a, b) = (unproject(a), unproject(b));
    let mean = (f64::from(a.lat_e7) + f64::from(b.lat_e7)) / 2.0 * 1e-7;
    let dy = f64::from(b.lat_e7) - f64::from(a.lat_e7);
    let dx = (f64::from(b.lon_e7) - f64::from(a.lon_e7)) * libm::cos(mean.to_radians());
    libm::sqrt(dx * dx + dy * dy) * M_PER_E7
}

/// The compass point of eight from `a` to `b`, north up.
pub fn compass(a: World, b: World) -> &'static str {
    let (a, b) = (unproject(a), unproject(b));
    let mean = (f64::from(a.lat_e7) + f64::from(b.lat_e7)) / 2.0 * 1e-7;
    let dy = f64::from(b.lat_e7) - f64::from(a.lat_e7);
    let dx = (f64::from(b.lon_e7) - f64::from(a.lon_e7)) * libm::cos(mean.to_radians());
    let deg = libm::atan2(dx, dy).to_degrees();
    let way = ((libm::round(deg) as i32).rem_euclid(360) + 22) / 45 % 8;
    ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][way as usize]
}

/// A distance as the radar says it: metres to the ten below a kilometre,
/// a tenth of a kilometre below ten, whole kilometres above.
pub fn distance(m: f64) -> String {
    match m {
        m if m < 995.0 => format!("{} m", (libm::round(m / 10.0) as i64) * 10),
        m if m < 10_000.0 => {
            let tenths = libm::round(m / 100.0) as i64;
            match tenths % 10 {
                0 => format!("{} km", tenths / 10),
                d => format!("{}.{d} km", tenths / 10),
            }
        }
        m => format!("{} km", libm::round(m / 1_000.0) as i64),
    }
}

/// Where `b` is from `a`: "1.3 km NE", "here" within ten metres.
pub fn relative(a: World, b: World) -> String {
    let m = metres(a, b);
    match m < 10.0 {
        true => String::from("here"),
        false => format!("{} {}", distance(m), compass(a, b)),
    }
}

/// The radar of a view `size` pixels across and down, a pixel spanning
/// `unit` world units: centred on our own pin, or on the newest peer's
/// without one; `None` without any pin.
pub fn radar(pins: &[MapPin], unit: f64, size: (u32, u32), lang: Lang) -> Option<Radar> {
    let (centre, on) = match pins.iter().find(|p| p.own) {
        Some(own) => (own.at, None),
        None => {
            let newest = pins
                .iter()
                .max_by_key(|p| (p.fix_at.unwrap_or(i64::MIN), core::cmp::Reverse(p.peer)))?;
            (newest.at, Some(newest.label.clone()))
        }
    };
    let per_px = metres_per_unit(centre) * unit;
    let half = f64::from(size.0.min(size.1)) / 2.0 * per_px;
    let step = ring_step(half / RINGS_IN_HALF);
    // Rings out to the screen's diagonal: a pan that puts the centre in a
    // corner still rings the whole screen.
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let reach = libm::sqrt(w * w + h * h) * per_px;
    let rings = (1..=MAX_RINGS)
        .map(|k| k * step)
        .take_while(|&r| f64::from(r) <= reach)
        .collect();
    Some(Radar {
        centre,
        on,
        step_m: step,
        rings,
        lang,
    })
}

/// `pins` as the radar labels them: each but the one it centres on says
/// where it is from the centre and how old its fix is, "1.3 km NE, 12 min
/// ago" (the age is the pin's own, already in the reader's words, 1447).
pub fn label(pins: &mut [MapPin], radar: &Radar) {
    for pin in pins.iter_mut() {
        if pin.at == radar.centre && (pin.own || radar.on.is_some()) {
            continue;
        }
        let way = relative(radar.centre, pin.at);
        pin.age = Some(match &pin.age {
            Some(age) => format!("{way}, {age}"),
            None => way,
        });
    }
}

/// The radar in the terminal's cells over `area`, `view` drawn at
/// `cell_px` image pixels a cell: each ring as dots (`·`, `.` in ASCII)
/// where it crosses a cell, its distance on its north crossing, the
/// centre as `+`, grey (no hue) and faint (level 1) under the tracks.
pub fn cells(
    radar: &Radar,
    view: &Viewport,
    area: Rect,
    cell_px: (u16, u16),
    ascii: bool,
) -> Vec<TrackCell> {
    let (cw, ch) = (f64::from(cell_px.0.max(1)), f64::from(cell_px.1.max(1)));
    let (cols, rows) = (i64::from(area.width), i64::from(area.height));
    let (cx, cy) = view.to_screen(radar.centre);
    let (cx, cy) = (cx as f64, cy as f64);
    let unit = libm::exp2(24.0 - f64::from(view.zoom()));
    let per_px = metres_per_unit(radar.centre) * unit;
    let mut taken = alloc::collections::BTreeSet::new();
    let mut out = Vec::new();
    let mut put = |col: i64, row: i64, text: String, level: u8, out: &mut Vec<TrackCell>| {
        let len = text.chars().count() as i64;
        if row < 0 || row >= rows || col < 0 || col + len > cols {
            return;
        }
        if (col..col + len).any(|c| taken.contains(&(c, row))) {
            return;
        }
        for c in col..col + len {
            taken.insert((c, row));
        }
        out.push(TrackCell {
            col: area.x + col as u16,
            row: area.y + row as u16,
            text,
            hue: None,
            level,
        });
    };
    let cell = |x: f64, y: f64| (libm::floor(x / cw) as i64, libm::floor(y / ch) as i64);
    let (c0, r0) = cell(cx, cy);
    put(c0, r0, String::from("+"), 2, &mut out);
    // The labels first, so a ring's dots go round them.
    for &r in &radar.rings {
        let px = f64::from(r) / per_px;
        let (col, row) = cell(cx, cy - px);
        let text = distance(f64::from(r));
        let len = text.chars().count() as i64;
        put(col - len / 2, row, text, 2, &mut out);
    }
    let dot = if ascii { "." } else { "·" };
    for &r in &radar.rings {
        let px = f64::from(r) / per_px;
        // A dot every second cell along the ring: faint, a ring and not a
        // wall, under the tracks.
        let n = (libm::ceil(core::f64::consts::TAU * px / cw / 2.0) as i64).clamp(12, 20_000);
        let mut last = None;
        for k in 0..n {
            let a = k as f64 / n as f64 * core::f64::consts::TAU;
            let at = cell(cx + px * libm::sin(a), cy - px * libm::cos(a));
            if last == Some(at) {
                continue;
            }
            last = Some(at);
            put(at.0, at.1, String::from(dot), 1, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use leviculum_map::mercator::{project, LatLon};

    fn pin(peer: u8, lat: f64, lon: f64, own: bool, fix_at: i64) -> MapPin {
        MapPin {
            peer: [peer; 16],
            at: project(LatLon::from_degrees(lat, lon)),
            label: if own {
                "you".to_string()
            } else {
                alloc::format!("p{peer}")
            },
            age: Some("12 min ago".to_string()),
            stale: false,
            nearby: false,
            own,
            fix_at: Some(fix_at),
            battery: None,
        }
    }

    #[test]
    fn the_ring_step_is_one_two_five() {
        assert_eq!(ring_step(0.3), 1);
        assert_eq!(ring_step(1.9), 1);
        assert_eq!(ring_step(4.0), 2);
        assert_eq!(ring_step(480.0), 200);
        assert_eq!(ring_step(500.0), 500);
        assert_eq!(ring_step(7_000.0), 5_000);
        assert_eq!(ring_step(12_000.0), 10_000);
    }

    /// The concept page's table: the step on a 480 x 320 window at the
    /// Seceda's latitude, by zoom.
    #[test]
    fn the_ring_step_by_zoom_is_the_pages_table() {
        let us = pin(1, 46.6006, 11.7254, true, 0);
        let step = |z: u8| {
            let unit = libm::exp2(24.0 - f64::from(z));
            radar(core::slice::from_ref(&us), unit, (480, 320), Lang::En).map(|r| r.step_m)
        };
        assert_eq!(
            [step(11), step(13), step(15), step(17)],
            [Some(2_000), Some(1_000), Some(200), Some(50)]
        );
    }

    #[test]
    fn distances_read_as_people_say_them() {
        assert_eq!(distance(3.0), "0 m");
        assert_eq!(distance(447.0), "450 m");
        assert_eq!(distance(999.9), "1 km");
        assert_eq!(distance(1_000.0), "1 km");
        assert_eq!(distance(1_290.0), "1.3 km");
        assert_eq!(distance(23_400.0), "23 km");
    }

    /// 1 km north-east of Florence's Duomo is NE and 1.0 km away.
    #[test]
    fn a_peer_north_east_is_named_by_distance_and_compass_point() {
        let a = project(LatLon::from_degrees(43.7731, 11.2560));
        let d = 1_000.0 / core::f64::consts::SQRT_2;
        let b = project(LatLon::from_degrees(
            43.7731 + d / 111_194.93,
            11.2560 + d / (111_194.93 * libm::cos(43.7731f64.to_radians())),
        ));
        assert_eq!(relative(a, b), "1 km NE");
        assert_eq!(compass(b, a), "SW");
        assert_eq!(relative(a, a), "here");
    }

    /// The radar centres on our own pin and labels every other one; with
    /// no own pin, on the newest peer, which it names in its note.
    #[test]
    fn the_radar_centres_on_us_or_on_the_newest_peer() {
        let pins = [
            pin(1, 43.78, 11.26, false, 100),
            pin(2, 43.7731, 11.2560, true, 50),
            pin(3, 43.77, 11.25, false, 300),
        ];
        let unit = libm::exp2(24.0 - 15.0);
        let r = radar(&pins, unit, (480, 320), Lang::En).expect("a radar");
        assert_eq!(r.centre, pins[1].at);
        assert!(r.on.is_none());
        assert!(!r.rings.is_empty() && r.rings.len() <= MAX_RINGS as usize);
        assert!(r.rings.windows(2).all(|w| w[1] - w[0] == r.step_m));
        let mut labelled = pins.to_vec();
        label(&mut labelled, &r);
        assert_eq!(
            labelled[1].age.as_deref(),
            Some("12 min ago"),
            "us unchanged"
        );
        let anna = labelled[0].age.clone().unwrap_or_default();
        assert_eq!(anna, "830 m NE, 12 min ago");

        let r = radar(
            &[pins[0].clone(), pins[2].clone()],
            unit,
            (480, 320),
            Lang::En,
        )
        .expect("a radar");
        assert_eq!(r.centre, pins[2].at, "the newest peer");
        assert_eq!(r.on.as_deref(), Some("p3"));
        assert_eq!(
            r.note(),
            "No map loaded for this area · Rings 200 m around p3"
        );
        let de = Radar {
            lang: Lang::De,
            on: None,
            ..r.clone()
        };
        assert_eq!(
            de.note(),
            "Keine Karte für diese Gegend geladen · Ringe 200 m"
        );
        assert!(radar(&[], unit, (480, 320), Lang::En).is_none());
    }

    /// Deeper than the outline and touched by no region: a radar; zoomed
    /// out to the outline, or over a region, the map.
    #[test]
    fn the_radar_shows_only_where_no_region_touches_the_view() {
        let at = project(LatLon::from_degrees(46.5, 9.0));
        let deep = Viewport::new(at, 14, 400, 300);
        let out = Viewport::new(at, 9, 400, 300);
        assert!(shows(&deep, &[]));
        assert!(!shows(&out, &[]));
    }
}
