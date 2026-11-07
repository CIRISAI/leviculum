//! The map's people (1200): the Friends sheet's rows, the avatar pins'
//! clusters, the distance to a friend and the `geo:` link Navigate hands
//! to the phone's navigation app.
//!
//! The sheet lists every peer with a position, newest first: those of the
//! last [`WINDOW_SECS`] as they are, the older ones after them, greyed
//! under a divider by the app (the reviewer's choice on the concept page,
//! 2026-10-31). A distance is given only with the phone's own fix; without
//! one the row has none, never one from the map's centre.
//!
//! The pins are avatar circles of [`PIN_DP`]; two whose centres lie closer
//! than one circle cluster ([`cluster`]): a pin joins the first cluster,
//! in the pins' order, whose first pin lies within a diameter of it, and
//! the cluster sits on that first pin. A cluster's tap zooms to where its
//! pins lie a diameter apart ([`split_zoom`]); where no zoom of the
//! regions does it, the app opens the sheet filtered to them.

use loquax::telemetry::Fix;
use loquax::ui::strings::Lang;
use loquax::ui::{chat, scene, UiModel};

use crate::schema::{self, FriendRow, VERSION};

/// The sheet's window: positions of the last day are its upper part.
pub const WINDOW_SECS: i64 = 86_400;

/// An avatar pin's diameter in density independent pixels: the chat
/// list's avatar is 48, the map's a little smaller so a town holds a few.
pub const PIN_DP: u32 = 40;

/// The Earth's mean radius in metres, as the haversine takes it.
const EARTH_M: f64 = 6_371_008.8;

/// Metres between two places, degrees times a million, along the sphere.
pub fn distance_m(a: (i32, i32), b: (i32, i32)) -> u32 {
    let rad = |e6: i32| (f64::from(e6) / 1e6).to_radians();
    let (la1, lo1, la2, lo2) = (rad(a.0), rad(a.1), rad(b.0), rad(b.1));
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    let metres = 2.0 * EARTH_M * h.sqrt().min(1.0).asin();
    metres.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// The `geo:` URI Navigate hands to the phone (1200): the place as the
/// centre and as the query, with the friend's name as the label, so a
/// navigation app shows a pin named after them. The label is percent
/// encoded: anything but letters, digits and `-._~` would end the query
/// or the label early.
pub fn geo_uri(lat_e6: i32, lon_e6: i32, label: &str) -> String {
    let place = format!("{},{}", degrees(lat_e6), degrees(lon_e6));
    let mut uri = format!("geo:{place}?q={place}");
    if !label.is_empty() {
        uri.push('(');
        for byte in label.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    uri.push(char::from(byte))
                }
                _ => uri.push_str(&format!("%{byte:02X}")),
            }
        }
        uri.push(')');
    }
    uri
}

/// Degrees times a million as a decimal with six places, the sign first.
fn degrees(e6: i32) -> String {
    let sign = if e6 < 0 { "-" } else { "" };
    let abs = e6.unsigned_abs();
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

/// The pins at `points` (screen pixels) grouped into clusters of circles
/// `diameter` wide, as the module says: each cluster its members' indices,
/// its first member the one it sits on, the clusters in the order of
/// their first members.
pub fn cluster(points: &[(f64, f64)], diameter: f64) -> Vec<Vec<usize>> {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for (i, &(x, y)) in points.iter().enumerate() {
        let near = clusters.iter_mut().find(|members| {
            let (ax, ay) = points[members[0]];
            (ax - x).hypot(ay - y) < diameter
        });
        match near {
            Some(members) => members.push(i),
            None => clusters.push(vec![i]),
        }
    }
    clusters
}

/// The zoom, in thousandths of a step, at which the pins `members` of
/// `points` (pixels at `zoom_milli`) lie at least `diameter` apart each,
/// with a tenth to spare: what a cluster's tap zooms to. `None` when that
/// is deeper than `max_milli`, or when two of them share one place, which
/// no zoom parts: the app opens the filtered sheet then.
pub fn split_zoom(
    points: &[(f64, f64)],
    members: &[usize],
    zoom_milli: u32,
    diameter: f64,
    max_milli: u32,
) -> Option<u32> {
    let mut closest = f64::INFINITY;
    for (n, &i) in members.iter().enumerate() {
        for &j in &members[n + 1..] {
            let (a, b) = (points[i], points[j]);
            closest = closest.min((a.0 - b.0).hypot(a.1 - b.1));
        }
    }
    if !closest.is_finite() {
        return Some(zoom_milli);
    }
    // Less than a hundredth of a pixel at the deepest zoom of the world is
    // one place.
    if closest < 1e-2 {
        return None;
    }
    let steps = (diameter * 1.1 / closest).log2().max(0.0);
    let at = f64::from(zoom_milli) + (steps * 1000.0).ceil();
    (at <= f64::from(max_milli)).then_some(at as u32)
}

/// The Friends sheet of `model` at the wall clock `now` (unix seconds),
/// with distances from `own` (degrees times a million) when the phone has
/// a fix: every peer with a position but ourselves and the blocked,
/// newest first, each with `recent` set within [`WINDOW_SECS`]; and our
/// own position apart as `me`, for the map's pin of ours. `photo` gives a
/// linked contact's thumbnail; the card's words are in `lang`. A node the
/// mesh announced that is no contact is a row as well, marked `nearby`
/// (1302).
pub fn friends<B>(
    model: &UiModel<B>,
    now: i64,
    own: Option<(i32, i32)>,
    lang: Lang,
    photo: impl Fn(&[u8; 16]) -> Option<String>,
) -> schema::Friends {
    let row = |peer: &[u8; 16], fix: &Fix| {
        let list = chat::list_row(model, peer);
        let at = fix.order_time();
        let place = (fix.location.latitude_e6, fix.location.longitude_e6);
        FriendRow {
            peer: loquax::address::to_hex(peer),
            geo: geo_uri(place.0, place.1, &list.name),
            name: list.name,
            initials: list.initials,
            icon_fg: list.icon.as_ref().map(|(fg, _)| fg.clone()),
            icon_bg: list.icon.map(|(_, bg)| bg),
            photo: photo(peer),
            at,
            lat_e6: place.0,
            lon_e6: place.1,
            distance_m: own.map(|own| distance_m(own, place)),
            recent: now.saturating_sub(at) <= WINDOW_SECS,
            stale: fix.stale(now),
            liveness: list.liveness.state.word().to_string(),
            cadence_s: crate::session::cadence(&list.liveness),
            last_fix_at: list.liveness.last_fix_at.unwrap_or(0),
            live: scene::row_live(&list.liveness, Some(now)).resolve(lang),
            age: scene::ago(now.saturating_sub(list.liveness.last_fix_at.unwrap_or(at)))
                .resolve(lang),
            ring: crate::session::ring(scene::Ring::of(list.liveness.state)),
            nearby: model.is_nearby(peer),
            announced: model.names.announced(peer).unwrap_or_default().to_string(),
        }
    };
    let mut rows: Vec<FriendRow> = model
        .positions
        .iter()
        .filter(|(peer, _)| Some(**peer) != model.address)
        .filter(|(peer, _)| model.names.trust(peer) != loquax::names::Trust::Blocked)
        .map(|(peer, fix)| row(peer, fix))
        .collect();
    // Newest first; the address breaks a tie, so the order never flickers.
    rows.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.peer.cmp(&b.peer)));
    let me = model
        .address
        .and_then(|own| model.positions.get(&own).map(|fix| row(&own, fix)));
    schema::Friends {
        v: VERSION,
        friends: rows,
        me,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1447 (walk 1 finding 7): a position 28 hours old has one age, in
    /// the phone's language, the same on the marker (`age`) and in the
    /// card (`live`), and never the word "fix".
    #[test]
    fn an_old_position_has_one_age_in_the_readers_words() {
        use loquax::ui::UiEvent;
        const NOW: i64 = 1_793_500_000;
        let mut model = UiModel::<crate::session::Line>::new(&[]);
        model.lang = Lang::De;
        update_clock(&mut model, NOW);
        let fix = leviculum_lxmf::telemetry::Location {
            latitude_e6: 43_773_100,
            longitude_e6: 11_256_000,
            altitude_e2: 0,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 0,
            last_update: NOW - 28 * 3600,
        };
        loquax::ui::update(
            &mut model,
            UiEvent::MeshPosition {
                identity: [1u8; 16],
                fix,
                name: Some("Walker".to_string()),
                age: 0,
            },
        );
        let sheet = friends(&model, NOW, None, Lang::De, |_| None);
        let row = sheet.friends.first().expect("a row");
        assert_eq!(row.age, "vor 28 Std.", "{row:?}");
        assert!(
            row.live.ends_with(&row.age),
            "the card says the same: {row:?}"
        );
        let pins = model.map_pins();
        assert!(
            pins.iter().all(|p| p.words().ends_with("vor 28 Std.")),
            "{pins:?}"
        );
        assert!(!row.live.contains("fix") && !row.age.contains("fix"));
    }

    /// 1302: a node the mesh announced is a row of the sheet, marked
    /// nearby with the name it announced and no avatar letters asked of
    /// the app; once it is added it is a friend's row. A peer known from
    /// a message is never nearby.
    #[test]
    fn a_node_the_mesh_announced_is_a_nearby_row_until_added() {
        use loquax::ui::UiEvent;
        const NOW: i64 = 1_793_500_000;
        let mut model = UiModel::<crate::session::Line>::new(&[]);
        update_clock(&mut model, NOW);
        let place = |lat: i32| leviculum_lxmf::telemetry::Location {
            latitude_e6: lat,
            longitude_e6: 11_256_000,
            altitude_e2: 0,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 0,
            last_update: NOW - 60,
        };
        for (identity, name, lat) in [
            ([1u8; 16], "Walker", 43_773_100),
            ([2u8; 16], "Rider", 43_774_100),
        ] {
            loquax::ui::update(
                &mut model,
                UiEvent::MeshPosition {
                    identity,
                    fix: place(lat),
                    name: Some(name.to_string()),
                    age: 0,
                },
            );
        }
        let sheet = friends(&model, NOW, None, Lang::En, |_| None);
        assert_eq!(sheet.friends.len(), 2, "{sheet:?}");
        assert!(sheet.friends.iter().all(|row| row.nearby));
        let mut announced: Vec<&str> = sheet.friends.iter().map(|r| r.announced.as_str()).collect();
        announced.sort_unstable();
        assert_eq!(announced, ["Rider", "Walker"]);
        let walker = loquax::mesh_position::delivery_of(&[1u8; 16]);
        loquax::ui::update(&mut model, UiEvent::AddNearby { peer: walker });
        let sheet = friends(&model, NOW, None, Lang::En, |_| None);
        let row = |peer: &[u8; 16]| {
            sheet
                .friends
                .iter()
                .find(|r| r.peer == loquax::address::to_hex(peer))
                .expect("a row")
        };
        assert!(!row(&walker).nearby, "added, a friend");
        assert!(row(&loquax::mesh_position::delivery_of(&[2u8; 16])).nearby);
    }

    /// The model's clock at `now`, unix seconds.
    fn update_clock(model: &mut UiModel<crate::session::Line>, now: i64) {
        loquax::ui::update(
            model,
            loquax::ui::UiEvent::Clock(loquax::when::WallTime {
                unix_secs: now,
                offset_secs: 0,
            }),
        );
    }

    #[test]
    fn the_geo_uri_names_the_place_twice_and_encodes_the_label() {
        assert_eq!(
            geo_uri(52_520_008, 13_404_954, "Anna"),
            "geo:52.520008,13.404954?q=52.520008,13.404954(Anna)"
        );
        // South and west, below one degree: the sign before the zero.
        assert_eq!(
            geo_uri(-500_000, -73_000_001, ""),
            "geo:-0.500000,-73.000001?q=-0.500000,-73.000001"
        );
        // A name with a space, a bracket, an ampersand and an umlaut ends
        // neither the query nor the label.
        assert_eq!(
            geo_uri(1, 2, "Jö (Rad) & Co"),
            "geo:0.000001,0.000002?q=0.000001,0.000002(J%C3%B6%20%28Rad%29%20%26%20Co)"
        );
    }

    #[test]
    fn the_distance_is_the_great_circle() {
        // Berlin Alexanderplatz to the Brandenburg Gate: 2.4 km.
        let d = distance_m((52_521_918, 13_413_215), (52_516_275, 13_377_704));
        assert!((2_400..2_500).contains(&d), "{d}");
        assert_eq!(distance_m((10, 20), (10, 20)), 0);
        // A quarter of the equator.
        let quarter = distance_m((0, 0), (0, 90_000_000));
        assert!((10_007_000..10_008_500).contains(&quarter), "{quarter}");
    }

    #[test]
    fn pins_closer_than_a_circle_cluster_on_the_first() {
        let points = [
            (100.0, 100.0),
            (130.0, 100.0),
            (300.0, 300.0),
            (60.0, 100.0),
        ];
        // 30 and 40 px from the first: both join it; 20 px from each other
        // does not matter, the first pin is the cluster's place.
        assert_eq!(cluster(&points, 41.0), vec![vec![0, 1, 3], vec![2]]);
        assert_eq!(
            cluster(&points, 40.0),
            vec![vec![0, 1], vec![2], vec![3]],
            "a diameter apart is apart"
        );
        assert!(cluster(&[], 40.0).is_empty());
    }

    #[test]
    fn a_cluster_splits_where_its_closest_pair_lies_a_circle_apart() {
        let points = [(0.0, 0.0), (10.0, 0.0), (0.0, 40.0)];
        // 10 px apart at z12, 44 wanted: two steps and a bit, 2.14 steps.
        let at = split_zoom(&points, &[0, 1, 2], 12_000, 40.0, 18_000).expect("splits");
        assert_eq!(at, 12_000 + 2_138);
        // Zoomed in by that, the closest pair is a circle and a tenth apart.
        let grown = 10.0 * 2f64.powf(f64::from(at - 12_000) / 1000.0);
        assert!((44.0..44.1).contains(&grown), "{grown}");
        // Too deep for the regions: no zoom, the sheet.
        assert_eq!(split_zoom(&points, &[0, 1], 17_000, 40.0, 18_000), None);
        // Two in one place never part.
        let same = [(5.0, 5.0), (5.0, 5.0)];
        assert_eq!(split_zoom(&same, &[0, 1], 3_000, 40.0, 18_000), None);
        // One pin is split already.
        assert_eq!(split_zoom(&points, &[2], 9_000, 40.0, 18_000), Some(9_000));
    }
}
