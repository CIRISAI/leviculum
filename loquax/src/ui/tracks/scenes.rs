//! Six synthetic scenes of tracks around Florence (1385), drawn through
//! the real drawing code as the pictures' goldens: in cells
//! (`tests/golden/tracks-*.txt`) and in the window's pixels
//! (`tests/golden/gui-tracks-*.png`). Each is a clock and a few peers'
//! fixes; nothing here draws. Five more for 1386's marks ([`marks`]): the
//! radar where no region is, the way back, a late stretch, a low battery,
//! and "fit all".

use alloc::vec::Vec;

use leviculum_map::mercator::World;

use super::TrackFix;
use crate::when::WallTime;

/// One scene: its name, the clock, and each peer with its name and its
/// fixes in time order; `own` our own address when one track is ours.
pub struct Scene {
    pub name: &'static str,
    pub now: WallTime,
    pub peers: Vec<([u8; 16], &'static str, Vec<TrackFix>)>,
    pub own: Option<[u8; 16]>,
    /// The battery each peer's newest fix reported, where one did (1386).
    pub battery: Vec<([u8; 16], u8)>,
}

impl Scene {
    /// Every fix's point: what the view is fitted to.
    pub fn points(&self) -> Vec<World> {
        self.peers
            .iter()
            .flat_map(|(_, _, fixes)| fixes.iter().map(TrackFix::at))
            .collect()
    }
}

/// 2026-11-04 12:00 CET.
pub fn noon() -> WallTime {
    WallTime::from_civil(2026, 11, 4, 12, 0, 3600)
}

/// Florence's Duomo, the scenes' origin.
const LAT: f64 = 43.7731;
const LON: f64 = 11.2560;

/// A fix at `t`, `north` and `east` metres from the origin, with an
/// accuracy of 5 m.
pub fn at(t: i64, north: f64, east: f64) -> TrackFix {
    let lat = LAT + north / 111_194.93;
    let lon = LON + east / (111_194.93 * libm::cos(LAT.to_radians()));
    TrackFix {
        t,
        lat_e6: libm::round(lat * 1e6) as i32,
        lon_e6: libm::round(lon * 1e6) as i32,
        speed_e2: 0,
        bearing_e2: 0,
        accuracy_e2: 500,
        late: false,
    }
}

/// A peer's address whose hue ([`super::hue`]) is `hue`, told apart by
/// `n`.
pub fn peer(hue: u8, n: u8) -> [u8; 16] {
    let mut p = [n; 16];
    p[0] = hue + 8 * n;
    p
}

/// The path through `points` (metres north and east) walked at `speed`
/// m/s, a fix every `every` seconds, from `start`.
fn walk(start: i64, points: &[(f64, f64)], speed: f64, every: i64) -> Vec<TrackFix> {
    let mut out = Vec::new();
    let mut t = start;
    let mut carry = 0.0;
    for w in points.windows(2) {
        let ((n0, e0), (n1, e1)) = (w[0], w[1]);
        let len = libm::sqrt((n1 - n0) * (n1 - n0) + (e1 - e0) * (e1 - e0));
        let step = speed * every as f64;
        let mut d = carry;
        while d < len {
            let k = d / len;
            out.push(at(t, n0 + (n1 - n0) * k, e0 + (e1 - e0) * k));
            t += every;
            d += step;
        }
        carry = d - len;
    }
    out
}

/// A stay at `(north, east)` from `start` for `secs`, a fix every
/// `every` seconds, jittering a few metres.
fn stay(start: i64, (north, east): (f64, f64), secs: i64, every: i64) -> Vec<TrackFix> {
    (0..=secs / every)
        .map(|i| {
            let j = ((i * 7) % 5) as f64 * 3.0;
            at(start + i * every, north + j, east - j)
        })
        .collect()
}

/// Shifted so its newest fix is at `end`.
fn ending(mut fixes: Vec<TrackFix>, end: i64) -> Vec<TrackFix> {
    if let Some(last) = fixes.last().map(|f| f.t) {
        for f in &mut fixes {
            f.t += end - last;
        }
    }
    fixes
}

/// A hiking group of four from the Duomo up to Fiesole, three hours,
/// a fix every two minutes; one turned back an hour ago and went quiet
/// (its pin's age says so), one leads at a brisk pace (the wedge).
pub fn hiking() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let route = [
        (0.0, 0.0),
        (900.0, 600.0),
        (1_800.0, 900.0),
        (2_700.0, 1_700.0),
        (3_600.0, 2_100.0),
        (4_300.0, 3_000.0),
    ];
    let spread =
        |d: f64| -> Vec<(f64, f64)> { route.iter().map(|&(a, b)| (a + d, b - d)).collect() };
    let start = n - 3 * 3600;
    let mut lead = walk(start, &spread(0.0), 0.62, 120);
    if let Some(last) = lead.last_mut() {
        last.speed_e2 = 170;
        last.bearing_e2 = 4_500;
    }
    let second = walk(start + 300, &spread(45.0), 0.55, 120);
    let third = walk(start + 600, &spread(-45.0), 0.5, 120);
    // Turned back at the second bend and has said nothing since 3 h.
    let mut back = walk(start, &spread(15.0)[..3], 0.6, 120);
    back.truncate(back.len().min(40));
    let back = ending(back, n - 3 * 3600 - 600);
    Scene {
        name: "hiking",
        now,
        peers: alloc::vec![
            (peer(0, 1), "anna", lead),
            (peer(1, 1), "bert", second),
            (peer(2, 1), "cleo", third),
            (peer(3, 1), "dino", back),
        ],
        own: None,
        battery: Vec::new(),
    }
}

/// A family in the Boboli gardens: three in a small area over two hours,
/// a fix a minute, half an hour at the playground (a dwell), loops that
/// cross.
pub fn park() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let (pn, pe) = (-1_180.0, -650.0);
    let loop_at = |r: f64, k: usize, phase: f64| -> Vec<(f64, f64)> {
        (0..=k)
            .map(|i| {
                let a = phase + i as f64 / k as f64 * core::f64::consts::TAU;
                (pn + r * libm::sin(a), pe + r * 1.4 * libm::cos(a))
            })
            .collect()
    };
    let mut mum = walk(n - 2 * 3600, &loop_at(160.0, 24, 0.0), 0.9, 60);
    let after = mum.last().map_or(n, |f| f.t + 60);
    mum.extend(stay(after, (pn + 40.0, pe + 20.0), 30 * 60, 60));
    let after = mum.last().map_or(n, |f| f.t + 60);
    mum.extend(walk(after, &loop_at(90.0, 16, 1.0), 0.8, 60));
    let mum = ending(mum, n - 60);
    let kid = ending(walk(n - 90 * 60, &loop_at(120.0, 20, 2.0), 1.6, 60), n - 30);
    let mut dad = stay(n - 100 * 60, (pn + 40.0, pe + 20.0), 45 * 60, 60);
    let after = dad.last().map_or(n, |f| f.t + 60);
    dad.extend(walk(after, &loop_at(200.0, 28, 3.0), 0.7, 60));
    let dad = ending(dad, n - 5 * 60);
    Scene {
        name: "park",
        now,
        peers: alloc::vec![
            (peer(4, 1), "mum", mum),
            (peer(5, 1), "pia", kid),
            (peer(0, 2), "you", dad),
        ],
        own: Some(peer(0, 2)),
        battery: Vec::new(),
    }
}

/// A convoy of ten on one road out of town, 90 km/h, 200 m apart, a fix
/// every 30 s for twenty minutes: ten wedges along one line.
pub fn convoy() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let road = [
        (-6_000.0, -9_000.0),
        (-2_000.0, -4_000.0),
        (1_000.0, 2_500.0),
    ];
    let mut peers = Vec::new();
    let names = [
        "car 1", "car 2", "car 3", "car 4", "car 5", "car 6", "car 7", "car 8", "car 9", "car 10",
    ];
    for (i, name) in names.iter().enumerate() {
        let mut fixes = walk(n - 9 * 60 - i as i64 * 8, &road, 25.0, 30);
        fixes.retain(|f| f.t <= n - 30);
        let before = fixes.iter().rev().nth(1).copied();
        if let (Some(b), Some(last)) = (before, fixes.last_mut()) {
            last.speed_e2 = 2_500;
            last.bearing_e2 = super::bearing_of(&b, last) * 100;
        }
        peers.push((peer(i as u8 % 8, 3 + i as u8 / 8), *name, fixes));
    }
    Scene {
        name: "convoy",
        now,
        peers,
        own: None,
        battery: Vec::new(),
    }
}

/// One peer with a trip a week old (an hour at a café, so a place) and a
/// walk this morning: the default window draws only the morning's lines.
pub fn week_old() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let week = n - 7 * 86_400;
    let mut old = walk(week, &[(0.0, 0.0), (-1_500.0, 1_800.0)], 1.2, 120);
    let after = old.last().map_or(week, |f| f.t + 120);
    old.extend(stay(after, (-1_500.0, 1_800.0), 3600, 300));
    let after = old.last().map_or(week, |f| f.t + 120);
    old.extend(walk(
        after,
        &[(-1_500.0, 1_800.0), (-200.0, 2_600.0)],
        1.2,
        120,
    ));
    let fresh = walk(
        n - 70 * 60,
        &[(400.0, -900.0), (900.0, -200.0), (700.0, 900.0)],
        0.6,
        120,
    );
    let mut fixes = old;
    fixes.extend(fresh);
    Scene {
        name: "week-old",
        now,
        peers: alloc::vec![(peer(6, 1), "ezra", fixes)],
        own: None,
        battery: Vec::new(),
    }
}

/// A walk with a fix every 20 s and one spike two kilometres off and
/// back, 100 m/s each way (an outlier, a dot), and one fix known only to
/// 600 m (off the line, a dot).
pub fn outlier() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let mut fixes = walk(n - 40 * 60, &[(-300.0, -800.0), (300.0, 900.0)], 0.8, 20);
    if let Some(f) = fixes.get_mut(45) {
        *f = at(f.t, 2_000.0, 300.0);
    }
    if let Some(f) = fixes.get_mut(81) {
        f.accuracy_e2 = 60_000;
        let moved = at(f.t, 450.0, 50.0);
        f.lat_e6 = moved.lat_e6;
        f.lon_e6 = moved.lon_e6;
    }
    Scene {
        name: "outlier",
        now,
        peers: alloc::vec![(peer(7, 1), "fede", fixes)],
        own: None,
        battery: Vec::new(),
    }
}

/// A year of a tracker in a handbag, a fix every 15 min: home at night,
/// work by day, a café on Saturdays, the commute between; today's
/// commute is the only line.
pub fn handbag() -> Scene {
    let now = noon();
    let end = now.unix_secs;
    let start = end - 365 * 86_400;
    let mut fixes = Vec::new();
    let mut t = start;
    let home = (-900.0, -1_300.0);
    let work = (1_100.0, 1_500.0);
    let cafe = (-2_000.0, 600.0);
    while t <= end {
        let local = t + 3600;
        let hour = local.rem_euclid(86_400) / 3600;
        let day = (local.div_euclid(86_400) + 3).rem_euclid(7);
        let jitter = ((t / 900) % 5) as f64 * 6.0;
        let (n0, e0) = match (day, hour) {
            (5, 10..=12) => cafe,
            (0..=4, 9..=16) => work,
            (0..=4, 8) | (0..=4, 17) => {
                let k = (local.rem_euclid(3600)) as f64 / 3600.0;
                let k = if hour == 8 { k } else { 1.0 - k };
                (
                    home.0 + (work.0 - home.0) * k,
                    home.1 + (work.1 - home.1) * k,
                )
            }
            _ => home,
        };
        fixes.push(at(t, n0 + jitter, e0 - jitter));
        t += 900;
    }
    Scene {
        name: "handbag",
        now,
        peers: alloc::vec![(peer(1, 2), "bag", fixes)],
        own: None,
        battery: Vec::new(),
    }
}

/// The six, in the report's order.
pub fn all() -> Vec<Scene> {
    alloc::vec![hiking(), park(), convoy(), week_old(), outlier(), handbag()]
}

/// The pins of a scene: each peer's newest fix, named, with its age.
pub fn pins(scene: &Scene) -> Vec<crate::ui::map::MapPin> {
    pins_in(scene, crate::ui::strings::Lang::En)
}

/// [`pins`] with the ages in `lang`, as a reader in that language sees
/// them (1451: the terminal's cells in German).
pub fn pins_in(scene: &Scene, lang: crate::ui::strings::Lang) -> Vec<crate::ui::map::MapPin> {
    let now = scene.now.unix_secs;
    scene
        .peers
        .iter()
        .filter_map(|(peer, name, fixes)| {
            let last = fixes.last()?;
            Some(crate::ui::map::MapPin {
                peer: *peer,
                at: last.at(),
                label: alloc::string::String::from(*name),
                age: Some(crate::ui::scene::ago(now - last.t).resolve(lang)),
                stale: now - last.t > crate::telemetry::STALE_AFTER_SECS,
                nearby: false,
                own: scene.own == Some(*peer),
                fix_at: Some(last.t),
                battery: scene
                    .battery
                    .iter()
                    .find(|(p, _)| p == peer)
                    .map(|(_, b)| *b),
            })
        })
        .collect()
}

/// `fixes` drawn about Florence moved to `(lat, lon)`, each the same
/// metres north and east of it.
fn moved_to(fixes: Vec<TrackFix>, (lat, lon): (f64, f64)) -> Vec<TrackFix> {
    let k = 111_194.93;
    fixes
        .into_iter()
        .map(|f| {
            let north = (f64::from(f.lat_e6) * 1e-6 - LAT) * k;
            let east = (f64::from(f.lon_e6) * 1e-6 - LON) * k * libm::cos(LAT.to_radians());
            TrackFix {
                lat_e6: libm::round((lat + north / k) * 1e6) as i32,
                lon_e6: libm::round((lon + east / (k * libm::cos(lat.to_radians()))) * 1e6) as i32,
                ..f
            }
        })
        .collect()
}

/// The Seceda ridge in the Dolomites, far from the Florence fixture: no
/// region is there, so the map draws the radar.
const SECEDA: (f64, f64) = (46.6006, 11.7254);

/// 1386, the radar: a walk up from a hut, ours, grey; Anna ahead to the
/// north-east, fresh; Bert behind to the south-west, his last fix 40 min
/// old. No region holds the ridge.
pub fn radar() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let own = walk(
        n - 2 * 3600,
        &[
            (-1_400.0, -900.0),
            (-900.0, -500.0),
            (-300.0, -200.0),
            (0.0, 0.0),
        ],
        0.35,
        120,
    );
    let anna = walk(
        n - 90 * 60,
        &[(-600.0, 200.0), (300.0, 500.0), (900.0, 900.0)],
        0.4,
        120,
    );
    let bert = ending(
        walk(
            n - 3 * 3600,
            &[(-2_600.0, -1_900.0), (-1_900.0, -1_500.0)],
            0.3,
            180,
        ),
        n - 40 * 60,
    );
    Scene {
        name: "radar",
        now,
        peers: alloc::vec![
            (peer(0, 2), "you", moved_to(own, SECEDA)),
            (peer(1, 4), "anna", moved_to(anna, SECEDA)),
            (peer(2, 4), "bert", moved_to(bert, SECEDA)),
        ],
        own: Some(peer(0, 2)),
        battery: Vec::new(),
    }
}

/// 1386, the way back: our own walk from the car park in the morning, a
/// fix a minute as the store keeps it, grey; nobody else.
pub fn way_back() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let own = ending(
        walk(
            n - 3 * 3600,
            &[
                (-1_800.0, 1_200.0),
                (-1_200.0, 900.0),
                (-900.0, 200.0),
                (-200.0, -300.0),
                (300.0, -100.0),
            ],
            0.45,
            60,
        ),
        n - 60,
    );
    Scene {
        name: "way-back",
        now,
        peers: alloc::vec![(peer(0, 2), "you", own)],
        own: Some(peer(0, 2)),
        battery: Vec::new(),
    }
}

/// 1386, a late stretch: a walk whose middle twenty minutes arrived an
/// hour and a half late, out of reach behind a hill: the segments those
/// fixes end are dashed.
pub fn late() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let mut fixes = walk(
        n - 70 * 60,
        &[
            (-900.0, -1_200.0),
            (-300.0, -200.0),
            (500.0, 300.0),
            (900.0, 1_100.0),
        ],
        0.8,
        60,
    );
    let (from, to) = (n - 45 * 60, n - 25 * 60);
    for f in fixes.iter_mut().filter(|f| f.t > from && f.t <= to) {
        f.late = true;
    }
    Scene {
        name: "late",
        now,
        peers: alloc::vec![(peer(3, 2), "gina", fixes)],
        own: None,
        battery: Vec::new(),
    }
}

/// 1386, a low battery: two walking, Kai's phone at 12 %, Lia's at 64 %.
pub fn battery() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let kai = walk(n - 30 * 60, &[(-500.0, -700.0), (200.0, -100.0)], 0.6, 60);
    let lia = walk(n - 30 * 60, &[(600.0, 900.0), (100.0, 300.0)], 0.6, 60);
    Scene {
        name: "battery",
        now,
        peers: alloc::vec![(peer(5, 2), "kai", kai), (peer(6, 2), "lia", lia)],
        own: None,
        battery: alloc::vec![(peer(5, 2), 12), (peer(6, 2), 64)],
    }
}

/// 1386, "fit all": two walking in town now, and Ugo, whose last fix is
/// five days old and 12 km away; the current episode holds the two, and
/// the fit leaves Ugo out.
pub fn fit_all() -> Scene {
    let now = noon();
    let n = now.unix_secs;
    let mara = walk(n - 40 * 60, &[(-400.0, -500.0), (100.0, 100.0)], 0.7, 60);
    let nico = walk(n - 40 * 60, &[(300.0, 600.0), (0.0, 200.0)], 0.6, 60);
    let ugo = ending(
        walk(n, &[(-12_000.0, 3_000.0), (-11_000.0, 3_500.0)], 1.0, 120),
        n - 5 * 86_400,
    );
    Scene {
        name: "fit-all",
        now,
        peers: alloc::vec![
            (peer(7, 2), "mara", mara),
            (peer(4, 2), "nico", nico),
            (peer(1, 5), "ugo", ugo),
        ],
        own: None,
        battery: Vec::new(),
    }
}

/// 1386's five, in the report's order.
pub fn marks() -> Vec<Scene> {
    alloc::vec![radar(), way_back(), late(), battery(), fit_all()]
}

/// The pins "fit all" brings into view in `scene` ([`super::inside`]).
pub fn fitted_pins(scene: &Scene) -> Vec<crate::ui::map::MapPin> {
    let inside = super::inside(&input(scene), super::TrackWindow::Episode, &scene.now);
    crate::ui::map::in_window(&pins(scene), &inside)
}

/// What [`super::tracks`] takes for a scene.
pub fn input(scene: &Scene) -> super::TrackInput {
    scene
        .peers
        .iter()
        .map(|(peer, _, fixes)| {
            (
                *peer,
                scene.own == Some(*peer),
                alloc::sync::Arc::new(fixes.clone()),
            )
        })
        .collect()
}

/// The view of `w` by `h` pixels that shows every fix of the scene with
/// `margin` to spare, no deeper than z17.
pub fn view_of(scene: &Scene, w: u32, h: u32, margin: u32) -> leviculum_map::viewport::Viewport {
    let points = scene.points();
    leviculum_map::viewport::Viewport::fit(&points, w, h, margin, 3..=17).unwrap_or_else(|| {
        leviculum_map::viewport::Viewport::new(
            World {
                x: 1 << 31,
                y: 1 << 31,
            },
            3,
            w,
            h,
        )
    })
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use alloc::string::String;
    use leviculum_ui::Rect;

    use crate::ui::map::{place, MapSetup};
    use crate::ui::strings::Lang;
    use crate::ui::tracks::{cells::cells, tracks, TrackWindow};

    /// The terminal's picture of a scene: braille cells of 72 by 22, the
    /// tracks and the pins over them, as the text frame draws them. The
    /// radar scene with its rings and its pins labelled from the centre,
    /// as [`crate::ui::UiModel::map_view`] lays them out where no region
    /// is (the cells hold none); "fit all" at the view that fit gives.
    /// The pins' words in `lang` (1451).
    fn frame_in(scene: &Scene, lang: Lang) -> String {
        let (cols, rows) = (72u16, 22u16);
        let area = Rect::new(0, 0, cols, rows);
        let setup = MapSetup {
            zooms: (0, 17),
            ..MapSetup::default()
        };
        let view = match scene.name {
            "fit-all" => {
                let (centre, zoom) = crate::ui::map::fit(&fitted_pins(scene), area, &setup);
                crate::ui::map::viewport(
                    &crate::ui::map::MapState {
                        centre,
                        zoom,
                        selected: None,
                    },
                    area,
                    &setup,
                )
            }
            _ => view_of(scene, u32::from(cols) * 2, u32::from(rows) * 4, 6),
        };
        let unit = libm::exp2(24.0 - f64::from(view.zoom()));
        let drawn = tracks(
            &input(scene),
            TrackWindow::Episode,
            &scene.now,
            unit,
            0.5,
            lang,
        );
        let mut pinned = pins_in(scene, lang);
        let radar = match scene.name == "radar" {
            true => crate::ui::radar::radar(&pinned, unit, view.size(), lang),
            false => None,
        };
        if let Some(r) = &radar {
            crate::ui::radar::label(&mut pinned, r);
        }
        let mut grid = alloc::vec![alloc::vec![' '; usize::from(cols)]; usize::from(rows)];
        let mut put = |col: u16, row: u16, text: &str| {
            for (i, ch) in text.chars().enumerate() {
                if let Some(cell) = grid
                    .get_mut(usize::from(row))
                    .and_then(|r| r.get_mut(usize::from(col) + i))
                {
                    *cell = ch;
                }
            }
        };
        if let Some(r) = &radar {
            for c in crate::ui::radar::cells(r, &view, area, setup.cell_px, false) {
                put(c.col, c.row, &c.text);
            }
        }
        for c in cells(&drawn, &view, area, setup.cell_px, false) {
            put(c.col, c.row, &c.text);
        }
        for p in place(&pinned, &view, area, &setup, None, false) {
            put(p.col, p.row, &p.text);
            if let Some(a) = &p.apart {
                put(a.col, a.row, &a.text);
            }
        }
        let mut out = String::new();
        out.push_str(&alloc::format!("{} z{}\n", scene.name, view.zoom()));
        if let Some(r) = &radar {
            out.push_str(&r.note());
            out.push('\n');
        }
        for row in grid {
            let line: String = row.into_iter().collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// The six scenes in cells, each the golden `tests/golden/tracks-
    /// <name>.txt`; `LOQUAX_WRITE_GOLDEN=1` writes them anew.
    #[test]
    fn the_track_scenes_in_cells_are_the_goldens() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
        for scene in all() {
            cells_are_the_golden(&dir, &scene);
        }
    }

    /// `scene`'s cells against `tracks-<name>.txt`, and in German, as a
    /// desktop under `LANG=de_DE.UTF-8` draws them, against
    /// `tracks-<name>-de.txt` (1451); `LOQUAX_WRITE_GOLDEN=1` writes both.
    fn cells_are_the_golden(dir: &std::path::Path, scene: &Scene) {
        for (lang, suffix) in [(Lang::En, ""), (Lang::De, "-de")] {
            let got = frame_in(scene, lang);
            let path = dir.join(alloc::format!("tracks-{}{suffix}.txt", scene.name));
            if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
                std::fs::write(&path, &got).expect("write golden");
            }
            let want = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(want == got, "{} moved:\n{got}", path.display());
        }
    }

    /// 1386's five scenes in cells, each the golden `tests/golden/tracks-
    /// <name>.txt`: the radar's rings, the grey way back, the late stretch
    /// in `┄`, the low battery's `▯`, "fit all" without the week-old pin.
    #[test]
    fn the_marks_scenes_in_cells_are_the_goldens() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
        for scene in marks() {
            cells_are_the_golden(&dir, &scene);
        }
    }

    /// "Fit all" leaves out the peer whose position is outside the time
    /// window, and takes every pin when none is inside.
    #[test]
    fn fit_all_takes_the_peers_inside_the_window() {
        let scene = fit_all();
        let names: Vec<String> = fitted_pins(&scene).into_iter().map(|p| p.label).collect();
        assert_eq!(names, ["mara", "nico"]);
        let old = crate::ui::map::in_window(&pins(&scene), &[]);
        assert_eq!(old.len(), 3);
    }

    /// The late stretch: its segments are late, the others not.
    #[test]
    fn a_late_stretch_is_dashed_and_the_rest_solid() {
        let scene = late();
        let view = view_of(&scene, 480, 320, 20);
        let unit = libm::exp2(24.0 - f64::from(view.zoom()));
        let t = &tracks(
            &input(&scene),
            TrackWindow::Episode,
            &scene.now,
            unit,
            1.0,
            Lang::En,
        )[0];
        let late = t.segments.iter().filter(|s| s.late).count();
        assert!(
            late > 0 && late < t.segments.len(),
            "{late} of {}",
            t.segments.len()
        );
    }

    /// The handbag's year is a few places and today's commute: inside
    /// the budget, and no line older than the episode.
    #[test]
    fn the_handbag_year_is_places_and_today() {
        let scene = handbag();
        assert!(scene.peers[0].2.len() > 35_000);
        let view = view_of(&scene, 480, 320, 20);
        let unit = libm::exp2(24.0 - f64::from(view.zoom()));
        let t = &tracks(
            &input(&scene),
            TrackWindow::Episode,
            &scene.now,
            unit,
            1.0,
            Lang::En,
        )[0];
        assert!(t.places.len() <= 8, "{:?}", t.places.len());
        assert!(t.places.len() >= 3);
        assert!(t.segments.len() < 200, "{}", t.segments.len());
    }
}
