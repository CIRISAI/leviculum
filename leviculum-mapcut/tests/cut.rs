//! The cut over synthetic vector tiles: deterministic whatever the thread
//! count or the order the source lists its tiles, the profile applied, and
//! the output a region the core reads.

use std::collections::BTreeMap;
use std::io::Cursor;

use leviculum_map::labels::LabelKind;
use leviculum_map::region::{Bounds, Region, TileId};
use leviculum_mapcut::cut::{
    cut, cut_tile, cut_tile_named, in_bounds, CutOptions, Names, DEVICE_POINTS,
};
use leviculum_mapcut::{Res, TileSource};
use leviculum_mapfill::fixture::Kind;
use leviculum_mapfill::style::Class;
use prost::Message;
use tinymvt::geometry::GeometryEncoder;
use tinymvt::vector_tile::tile::{Feature, Layer, Value};
use tinymvt::vector_tile::Tile;

const POLYGON: i32 = 3;
const LINESTRING: i32 = 2;
const POINT: i32 = 1;

fn square(x: i32, y: i32, side: i32) -> Vec<[i32; 2]> {
    // Clockwise on screen, positive area: an outer ring.
    vec![[x, y], [x + side, y], [x + side, y + side], [x, y + side]]
}

fn layer(name: &str, features: Vec<(i32, &str, Vec<u32>)>) -> Layer {
    let mut values = Vec::new();
    let mut fs = Vec::new();
    for (t, kind, geometry) in features {
        let tags = if kind.is_empty() {
            vec![]
        } else {
            values.push(Value {
                string_value: Some(kind.to_owned()),
                ..Value::default()
            });
            vec![0, values.len() as u32 - 1]
        };
        fs.push(Feature {
            id: None,
            tags,
            r#type: Some(t),
            geometry,
        });
    }
    Layer {
        version: 2,
        name: name.to_owned(),
        features: fs,
        keys: vec!["kind".to_owned()],
        values,
        extent: Some(4096),
    }
}

fn polygon(rings: &[Vec<[i32; 2]>]) -> Vec<u32> {
    let mut e = GeometryEncoder::new();
    for r in rings {
        e.add_ring(r.iter().copied());
    }
    e.into_vec()
}

fn line(pts: &[[i32; 2]]) -> Vec<u32> {
    let mut e = GeometryEncoder::new();
    e.add_linestring(pts.iter().copied());
    e.into_vec()
}

/// A tile with one of each thing the profile acts on.
fn busy_tile() -> Vec<u8> {
    let mut hole = square(1000, 1000, 500);
    hole.reverse();
    // A straight road with 50 collinear vertices.
    let road: Vec<[i32; 2]> = (0..50).map(|i| [i * 80, 2000]).collect();
    // A long wiggly path over the device's capacity.
    let wiggle: Vec<[i32; 2]> = (0..(DEVICE_POINTS as i32 + 500))
        .map(|i| [i % 4000, (i / 4000) * 100 + (i % 2) * 40])
        .collect();
    Tile {
        layers: vec![
            layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
            layer(
                "landuse",
                vec![
                    (POLYGON, "park", polygon(&[square(500, 500, 2000), hole])),
                    // Three pixels square at z15: thinned only at region zooms.
                    (POLYGON, "park", polygon(&[square(3000, 3000, 48)])),
                ],
            ),
            layer(
                "buildings",
                vec![(POLYGON, "building", polygon(&[square(100, 100, 50)]))],
            ),
            layer(
                "roads",
                vec![
                    (LINESTRING, "major_road", line(&road)),
                    (LINESTRING, "path", line(&wiggle)),
                ],
            ),
            layer(
                "water",
                vec![(LINESTRING, "river", line(&[[0, 10], [4096, 30]]))],
            ),
            layer(
                "pois",
                vec![(POINT, "cafe", {
                    let mut e = GeometryEncoder::new();
                    e.add_points([[5, 5]]);
                    e.into_vec()
                })],
            ),
        ],
    }
    .encode_to_vec()
}

fn sea_tile() -> Vec<u8> {
    Tile {
        layers: vec![layer(
            "water",
            vec![(POLYGON, "ocean", polygon(&[square(0, 0, 4096)]))],
        )],
    }
    .encode_to_vec()
}

struct Synthetic {
    tiles: BTreeMap<TileId, Vec<u8>>,
    reversed: bool,
}

impl TileSource for Synthetic {
    /// Every tile, the box and the zooms ignored: the cut filters them
    /// itself, which `tiles_outside_the_box_or_the_zooms_are_left_out`
    /// holds it to.
    fn tiles_in(&self, _: &Bounds, _: std::ops::RangeInclusive<u8>) -> Res<Vec<TileId>> {
        let mut t: Vec<TileId> = self.tiles.keys().copied().collect();
        if self.reversed {
            t.reverse();
        }
        Ok(t)
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        Ok(self.tiles.get(&t).cloned())
    }
}

/// A feature with properties: geometry type, its commands, key-value pairs.
type Tagged<'a> = (i32, Vec<u32>, Vec<(&'a str, Value)>);

/// A layer whose features carry properties.
fn tagged(name: &str, features: Vec<Tagged<'_>>) -> Layer {
    let mut keys: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    let mut fs = Vec::new();
    for (t, geometry, props) in features {
        let mut tags = Vec::new();
        for (k, v) in props {
            let ki = keys.iter().position(|x| x == k).unwrap_or_else(|| {
                keys.push(k.to_owned());
                keys.len() - 1
            });
            values.push(v);
            tags.extend([ki as u32, values.len() as u32 - 1]);
        }
        fs.push(Feature {
            id: None,
            tags,
            r#type: Some(t),
            geometry,
        });
    }
    Layer {
        version: 2,
        name: name.to_owned(),
        features: fs,
        keys,
        values,
        extent: Some(4096),
    }
}

fn text(s: &str) -> Value {
    Value {
        string_value: Some(s.to_owned()),
        ..Value::default()
    }
}

fn number(v: i64) -> Value {
    Value {
        int_value: Some(v),
        ..Value::default()
    }
}

fn at(x: i32, y: i32) -> Vec<u32> {
    let mut e = GeometryEncoder::new();
    e.add_points([[x, y]]);
    e.into_vec()
}

/// A Protomaps 4.x tile with one place of every class the source has, a
/// water and a park point, a cafe, and named roads of every kind.
fn named_tile() -> Vec<u8> {
    let place = |kind: &str, detail: &str, name: &str, x: i32, min_zoom: i64| {
        (
            POINT,
            at(x, 100),
            vec![
                ("kind", text(kind)),
                ("kind_detail", text(detail)),
                ("name", text(name)),
                ("min_zoom", number(min_zoom)),
                ("population_rank", number(x as i64 / 100)),
            ],
        )
    };
    let road = |kind: &str, name: &str, y: i32| {
        (
            LINESTRING,
            line(&[[0, y], [4000, y]]),
            vec![("kind", text(kind)), ("name", text(name))],
        )
    };
    Tile {
        layers: vec![
            layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
            tagged(
                "places",
                vec![
                    {
                        let mut p = place("locality", "city", "Frankfurt am Main", 1200, 4);
                        p.2.push(("name:de", text("Frankfurt am Main")));
                        p.2.push(("name:en", text("Frankfurt")));
                        p
                    },
                    place("locality", "town", "Offenbach am Main", 1100, 8),
                    place("macrohood", "quarter", "Innenstadt I", 1000, 11),
                    place("locality", "village", "Altenhain", 900, 11),
                    place("neighbourhood", "suburb", "Sachsenhausen", 800, 13),
                    place("neighbourhood", "neighbourhood", "Westend", 700, 13),
                    // Left out: field names, hamlets, single houses, farms.
                    place("locality", "locality", "Am Kirchberg", 600, 13),
                    place("locality", "hamlet", "Hof Hausen", 500, 12),
                    place("locality", "isolated_dwelling", "Forsthaus", 400, 13),
                    place("locality", "farm", "Gut Neuhof", 300, 13),
                    // A suburb a z14 view shows, not this z13 tile.
                    place("neighbourhood", "suburb", "Gallus", 200, 14),
                    // In the buffer: its neighbour's.
                    (
                        POINT,
                        at(-20, 100),
                        vec![
                            ("kind", text("locality")),
                            ("kind_detail", text("city")),
                            ("name", text("Mainz")),
                        ],
                    ),
                    // Not a name: an empty one.
                    place("locality", "town", " ", 150, 8),
                ],
            ),
            tagged(
                "water",
                vec![
                    (
                        POINT,
                        at(2000, 2000),
                        vec![("kind", text("water")), ("name", text("Main"))],
                    ),
                    (POINT, at(2100, 2000), vec![("kind", text("water"))]),
                    (
                        POLYGON,
                        polygon(&[square(1000, 1000, 500)]),
                        vec![("kind", text("water")), ("name", text("Teich"))],
                    ),
                ],
            ),
            tagged(
                "pois",
                vec![
                    (
                        POINT,
                        at(3000, 3000),
                        vec![
                            ("kind", text("park")),
                            ("name", text("Grüneburgpark")),
                            ("name:de", text("Grüneburgpark")),
                        ],
                    ),
                    (
                        POINT,
                        at(3100, 3000),
                        vec![("kind", text("cafe")), ("name", text("Café Laumer"))],
                    ),
                ],
            ),
            tagged(
                "roads",
                vec![
                    road("highway", "A 66", 500),
                    road("major_road", "Mainzer Landstraße", 600),
                    road("medium_road", "Eschersheimer Landstraße", 700),
                    road("minor_road", "Wolfsgangstraße", 800),
                    road("path", "Mainuferweg", 900),
                    road("rail", "Main-Weser-Bahn", 1000),
                    (
                        LINESTRING,
                        line(&[[0, 1100], [4000, 1100]]),
                        vec![("kind", text("minor_road"))],
                    ),
                ],
            ),
        ],
    }
    .encode_to_vec()
}

/// Around Santa Maria del Fiore: the z15 tiles 17408..=17409 x 11943.
const BOX: Bounds = Bounds {
    west: 112_505_000,
    south: 437_728_000,
    east: 112_700_000,
    north: 437_785_000,
};

fn source(reversed: bool) -> Synthetic {
    let mut tiles = BTreeMap::new();
    let t = |z, x, y| TileId { z, x, y };
    tiles.insert(t(15, 17408, 11943), busy_tile());
    // Two sea tiles with the same bytes: stored once.
    tiles.insert(t(15, 17409, 11943), sea_tile());
    tiles.insert(t(14, 8704, 5971), sea_tile());
    tiles.insert(t(12, 2176, 1492), busy_tile());
    tiles.insert(t(13, 4352, 2985), named_tile());
    // Outside the box, and outside the zoom range.
    tiles.insert(t(15, 17500, 11943), busy_tile());
    tiles.insert(t(9, 272, 186), busy_tile());
    Synthetic { tiles, reversed }
}

fn options(threads: usize) -> CutOptions {
    CutOptions {
        bounds: BOX,
        zooms: 10..=15,
        profile: 1,
        attribution: "© OpenStreetMap contributors".to_owned(),
        notice: "test notice, ODbL".to_owned(),
        threads,
        locale: Some("de".to_owned()),
        buildings: true,
        pois: true,
        round: leviculum_mapcut::cut::ROUND,
    }
}

fn run(src: &Synthetic, threads: usize) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    cut(src, &options(threads), &mut out).expect("cut");
    out.into_inner()
}

#[test]
fn the_cut_is_deterministic() {
    let a = run(&source(false), 1);
    let b = run(&source(true), 4);
    let c = run(&source(false), 7);
    assert_eq!(a, b);
    assert_eq!(a, c);
}

#[test]
fn the_cut_applies_profile_one_and_reads_back() {
    let bytes = run(&source(false), 3);
    let r = Region::from_bytes(&bytes).expect("open");
    r.verify().expect("verify");
    let ids: Vec<TileId> = (0..r.header().tiles)
        .map(|n| r.entry(n).expect("entry").id)
        .collect();
    assert_eq!(
        ids,
        [
            (12, 2176, 1492),
            (13, 4352, 2985),
            (14, 8704, 5971),
            (15, 17408, 11943),
            (15, 17409, 11943)
        ]
        .map(|(z, x, y)| TileId { z, x, y })
    );
    assert_eq!(r.attribution(), "© OpenStreetMap contributors");
    // The two sea tiles share one blob.
    let sea14 = r.entry(2).expect("entry");
    let sea15 = r.entry(4).expect("entry");
    assert_eq!((sea14.offset, sea14.len), (sea15.offset, sea15.len));

    let classes = |id: TileId| -> Vec<(Class, usize)> {
        let fx = r.fixture(id).expect("tile").expect("present");
        fx.features()
            .map(|f| (f.class, f.rings().map(|r| r.len()).sum()))
            .collect()
    };
    // z15: no building, no point; the park keeps its hole and the small
    // park stays; the straight road is two vertices; the long path is cut
    // into two features the device holds, sharing an end point; the river
    // is a line.
    let z15 = classes(TileId {
        z: 15,
        x: 17408,
        y: 11943,
    });
    let names: Vec<Class> = z15.iter().map(|c| c.0).collect();
    assert_eq!(
        names,
        [
            Class::Earth,
            Class::Park,
            Class::Park,
            Class::Waterway,
            Class::Path,
            Class::Path,
            Class::SecondaryRoad
        ]
    );
    assert_eq!(z15[1].1, 8, "outer ring and hole");
    assert_eq!(z15[6].1, 2, "the straight road");
    assert_eq!(z15[4].1, DEVICE_POINTS);
    assert!((1..DEVICE_POINTS).contains(&z15[5].1));
    let fx = r
        .fixture(TileId {
            z: 15,
            x: 17408,
            y: 11943,
        })
        .expect("tile")
        .expect("present");
    assert_eq!(fx.shift(), 0);
    assert!(fx.features().all(|f| f.class != Class::Building));
    let path = fx
        .features()
        .find(|f| f.class == Class::Path)
        .expect("path");
    assert_eq!(path.kind, Kind::Line);

    // z12, a region zoom: extent 512, the small park thinned away.
    let z12 = r
        .fixture(TileId {
            z: 12,
            x: 2176,
            y: 1492,
        })
        .expect("tile")
        .expect("present");
    assert_eq!(z12.shift(), 3);
    let parks = z12.features().filter(|f| f.class == Class::Park).count();
    assert_eq!(parks, 1);
}

/// Forest apart from parks, the three road steps, the railways 4.x
/// carries among the roads, and bridges and tunnels in both schemas the
/// tree meets: `is_bridge` and `is_tunnel`
/// (4.x) and `pmap:level` (3.x). The class byte alone, all a reader before
/// 1099 reads, still says park and major road.
#[test]
fn the_cut_keeps_forest_road_steps_bridges_and_tunnels() {
    let text = |s: &str| Value {
        string_value: Some(s.to_owned()),
        ..Value::default()
    };
    let yes = Value {
        bool_value: Some(true),
        ..Value::default()
    };
    let roads = Layer {
        version: 2,
        name: "roads".to_owned(),
        features: [
            (vec![0, 0, 1, 3], [100, 100]),
            (vec![0, 1, 2, 4], [100, 200]),
            (vec![0, 2], [100, 300]),
            (vec![0, 0, 3, 5], [100, 400]),
            (vec![0, 6], [100, 500]),
        ]
        .into_iter()
        .map(|(tags, [x, y])| Feature {
            id: None,
            tags,
            r#type: Some(LINESTRING),
            geometry: line(&[[x, y], [x + 1000, y]]),
        })
        .collect(),
        keys: ["kind", "is_bridge", "is_tunnel", "pmap:level"]
            .map(str::to_owned)
            .to_vec(),
        values: vec![
            text("highway"),
            text("medium_road"),
            text("minor_road"),
            yes.clone(),
            yes,
            Value {
                sint_value: Some(-1),
                ..Value::default()
            },
            text("rail"),
        ],
        extent: Some(4096),
    };
    let tile = Tile {
        layers: vec![
            layer(
                "landuse",
                vec![
                    (POLYGON, "park", polygon(&[square(0, 0, 1000)])),
                    (POLYGON, "wood", polygon(&[square(2000, 0, 1000)])),
                    (POLYGON, "meadow", polygon(&[square(0, 2000, 1000)])),
                ],
            ),
            roads,
        ],
    }
    .encode_to_vec();
    let layers = leviculum_map::profile::layers(1, 15).expect("profile");
    let (blob, _) = cut_tile(&tile, layers).expect("cut");
    let blob = blob.expect("a blob");
    let fx = leviculum_mapfill::fixture::Fixture::parse(&blob).expect("parse");
    let got: Vec<_> = fx
        .features()
        .map(|f| (f.class, f.bridge(), f.tunnel()))
        .collect();
    assert_eq!(
        got,
        [
            (Class::Forest, false, false),
            (Class::Park, false, false),
            (Class::Park, false, false),
            (Class::MinorRoad, false, false),
            (Class::SecondaryRoad, false, true),
            (Class::MajorRoad, true, false),
            (Class::MajorRoad, false, true),
            (Class::Rail, false, false),
        ]
    );
    // An old reader's view: the class bytes alone, in order.
    let mut bytes = Vec::new();
    let mut at = 12 + 8 + 4;
    for f in fx.features() {
        bytes.push(blob[at + 1]);
        at += 6 + f.rings().map(|r| 2 + 4 * r.len()).sum::<usize>();
    }
    assert_eq!(bytes, [2, 2, 2, 18, 19, 19, 19, 20]);
}

#[test]
fn tiles_outside_the_box_or_the_zooms_are_left_out() {
    let t = |z, x, y| TileId { z, x, y };
    assert!(in_bounds(t(15, 17408, 11943), &BOX));
    assert!(!in_bounds(t(15, 17500, 11943), &BOX));
    assert!(!in_bounds(t(15, 17408, 11000), &BOX));
    assert!(in_bounds(t(0, 0, 0), &BOX));
    // A box across the antimeridian holds both edge columns.
    let across = Bounds {
        west: 1_790_000_000,
        south: -10_000_000,
        east: -1_790_000_000,
        north: 10_000_000,
    };
    assert!(in_bounds(t(5, 31, 15), &across));
    assert!(in_bounds(t(5, 0, 16), &across));
    assert!(!in_bounds(t(5, 16, 15), &across));
}

#[test]
fn a_tile_with_nothing_left_is_not_written() {
    let only_buildings = Tile {
        layers: vec![layer(
            "buildings",
            vec![(POLYGON, "building", polygon(&[square(10, 10, 100)]))],
        )],
    }
    .encode_to_vec();
    let layers = leviculum_map::profile::layers(1, 15).expect("profile");
    let (blob, st) = cut_tile(&only_buildings, layers).expect("cut");
    assert!(blob.is_none());
    assert_eq!((st.empty, st.features_in, st.features_out), (1, 1, 0));
}

#[test]
fn the_cut_names_exactly_the_classes_the_map_labels() {
    let layers = leviculum_map::profile::layers(1, 13).expect("profile");
    let names = |z: u8, deepest: bool| Names {
        locale: Some("en"),
        z,
        deepest,
    };
    let (blob, labels, st) =
        cut_tile_named(&named_tile(), layers, Some(names(13, false))).expect("cut");
    let blob = blob.expect("a blob");
    let points: Vec<(LabelKind, u8, &str, Option<&str>)> = labels
        .points
        .iter()
        .map(|(k, rank, _, n, _)| (*k, *rank, n.local.as_str(), n.alternate.as_deref()))
        .collect();
    assert_eq!(
        points,
        [
            (LabelKind::City, 12, "Frankfurt am Main", Some("Frankfurt")),
            (LabelKind::Town, 11, "Offenbach am Main", None),
            (LabelKind::District, 10, "Innenstadt I", None),
            (LabelKind::Village, 9, "Altenhain", None),
            (LabelKind::Suburb, 8, "Sachsenhausen", None),
            (LabelKind::Neighbourhood, 7, "Westend", None),
            (LabelKind::Water, 0, "Main", None),
            (LabelKind::Park, 0, "Grüneburgpark", None),
        ]
    );
    // No street names in a z13 tile that is not the region's deepest.
    assert!(labels.lines.is_empty());
    assert_eq!((st.point_labels, st.line_labels), (8, 0));

    // At z14, and in the deepest zoom of a region whatever it is: the
    // suburb a z14 view shows, and the named roads, paths and railways
    // not among them, each pointing at its own line feature.
    for (z, deepest) in [(14, false), (13, true)] {
        let (blob, labels, _) =
            cut_tile_named(&named_tile(), layers, Some(names(z, deepest))).expect("cut");
        let blob = blob.expect("a blob");
        assert!(labels.points.iter().any(|p| p.3.local == "Gallus"), "z{z}");
        let fx = leviculum_mapfill::fixture::Fixture::parse(&blob).expect("parse");
        let features: Vec<_> = fx.features().collect();
        let lines: Vec<(LabelKind, u8, &str, Class)> = labels
            .lines
            .iter()
            .map(|(f, k, mz, n)| {
                let f = &features[usize::from(*f)];
                assert_eq!(f.kind, Kind::Line);
                (*k, *mz, n.local.as_str(), f.class)
            })
            .collect();
        assert_eq!(
            lines,
            [
                (
                    LabelKind::MinorRoad,
                    15,
                    "Wolfsgangstraße",
                    Class::MinorRoad
                ),
                (
                    LabelKind::SecondaryRoad,
                    14,
                    "Mainzer Landstraße",
                    Class::SecondaryRoad
                ),
                (
                    LabelKind::SecondaryRoad,
                    14,
                    "Eschersheimer Landstraße",
                    Class::SecondaryRoad
                ),
                (LabelKind::MajorRoad, 14, "A 66", Class::MajorRoad),
            ],
            "z{z}"
        );
    }
    // The geometry is the same with names as without.
    let (plain, _) = cut_tile(&named_tile(), layers).expect("cut");
    assert_eq!(plain.as_deref(), Some(blob.as_slice()));
}

#[test]
fn a_region_carries_its_names_and_one_string_table() {
    let bytes = run(&source(false), 2);
    let r = Region::from_bytes(&bytes).expect("open");
    r.verify().expect("verify");
    let l = r.header().labels.expect("version 2");
    assert_eq!(l.sections, 1, "only the named tile has a section");
    let id = TileId {
        z: 13,
        x: 4352,
        y: 2985,
    };
    let s = r.labels(id).expect("labels").expect("a section");
    let first = s.points().next().expect("a point").1;
    assert_eq!(r.string(first.string), Ok("Frankfurt am Main"));
    // `de` is the cut's locale here, and the German name is the local one.
    assert_eq!(s.alternate(0), None);
    assert!(r.find_string("Grüneburgpark").expect("search").is_some());
    assert!(r.find_string("Am Kirchberg").expect("search").is_none());
    for i in 1..r.string_count() {
        assert!(r.string(i - 1).expect("s") < r.string(i).expect("s"));
    }
}

/// A Protomaps 4.x tile with the waters the map names (1126): a river, a
/// canal and a stream as lines of `water` with their own `min_zoom`, a
/// drain and an unnamed river it does not name, the river's unnamed area,
/// and a lake named as a `pois` point of kind `water`.
fn waters_tile() -> Vec<u8> {
    let water = |kind: &str, name: Option<&str>, y: i32, min_zoom: i64| {
        let mut props = vec![("kind", text(kind)), ("min_zoom", number(min_zoom))];
        props.extend(name.map(|n| ("name", text(n))));
        (
            LINESTRING,
            line(&[[0, y], [2000, y + 300], [4000, y]]),
            props,
        )
    };
    Tile {
        layers: vec![
            layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
            tagged(
                "water",
                vec![
                    (
                        POLYGON,
                        polygon(&[square(0, 400, 4096)]),
                        vec![("kind", text("water")), ("kind_detail", text("river"))],
                    ),
                    water("river", Some("Main"), 500, 10),
                    water("canal", Some("Osthafenkanal"), 1500, 12),
                    water("stream", Some("Luderbach"), 2500, 14),
                    water("drain", Some("Graben"), 3000, 14),
                    water("river", None, 3500, 10),
                ],
            ),
            tagged(
                "pois",
                vec![(
                    POINT,
                    at(1000, 3800),
                    vec![
                        ("kind", text("water")),
                        ("name", text("Jacobiweiher")),
                        ("min_zoom", number(12)),
                    ],
                )],
            ),
        ],
    }
    .encode_to_vec()
}

/// Rivers are named (1126): the Main carried its name on the line of
/// `water` alone, which the cut did not label, and its area has none.
/// Rivers, canals and streams are labelled from their own `min_zoom`, also
/// below the street names' z14, and in the deepest zoom; a drain is not;
/// a lake named as a `pois` point is a water name.
#[test]
fn the_cut_names_rivers_canals_and_streams_along_their_lines() {
    let layers = leviculum_map::profile::layers(1, 12).expect("profile");
    for (z, deepest, want) in [
        (11, false, vec![("Main", 10)]),
        (12, false, vec![("Main", 10), ("Osthafenkanal", 12)]),
        (
            12,
            true,
            vec![("Main", 10), ("Osthafenkanal", 12), ("Luderbach", 14)],
        ),
    ] {
        let names = Names {
            locale: Some("de"),
            z,
            deepest,
        };
        let (blob, labels, st) = cut_tile_named(&waters_tile(), layers, Some(names)).expect("cut");
        let blob = blob.expect("blob");
        let fx = leviculum_mapfill::fixture::Fixture::parse(&blob).expect("parse");
        let features: Vec<_> = fx.features().collect();
        let lines: Vec<(&str, u8)> = labels
            .lines
            .iter()
            .map(|(f, k, mz, n)| {
                let f = &features[usize::from(*f)];
                assert_eq!(
                    (f.kind, f.class, *k),
                    (Kind::Line, Class::Waterway, LabelKind::River)
                );
                (n.local.as_str(), *mz)
            })
            .collect();
        assert_eq!(lines, want, "z{z} deepest {deepest}");
        let points: Vec<(LabelKind, &str)> = labels
            .points
            .iter()
            .map(|p| (p.0, p.3.local.as_str()))
            .collect();
        let lake = match z >= 12 {
            true => vec![(LabelKind::Water, "Jacobiweiher")],
            false => vec![],
        };
        assert_eq!(points, lake, "z{z}");
        assert_eq!(st.line_labels, want.len() as u64);
    }
}

/// A named line the device cannot hold whole is cut into pieces, and every
/// piece carries the name, so the reader joins them back into one line
/// (1126); before, the largest piece alone was named.
#[test]
fn every_piece_of_a_long_named_line_carries_its_name() {
    let layers = leviculum_map::profile::layers(1, 15).expect("profile");
    // A zigzag no simplification shortens, longer than the device holds.
    let n = DEVICE_POINTS as i32 + 500;
    let pts: Vec<[i32; 2]> = (0..n)
        .map(|i| [i * 2 - 4000, 1000 + (i % 2) * 400])
        .collect();
    let tile = Tile {
        layers: vec![tagged(
            "roads",
            vec![(
                LINESTRING,
                line(&pts),
                vec![("kind", text("minor_road")), ("name", text("Lange Straße"))],
            )],
        )],
    }
    .encode_to_vec();
    let names = Names {
        locale: None,
        z: 15,
        deepest: true,
    };
    let (blob, labels, _) = cut_tile_named(&tile, layers, Some(names)).expect("cut");
    let blob = blob.expect("blob");
    let fx = leviculum_mapfill::fixture::Fixture::parse(&blob).expect("parse");
    let pieces = fx.features().count();
    assert!(pieces >= 2, "{pieces} pieces");
    assert_eq!(labels.lines.len(), pieces);
    assert!(labels.lines.iter().all(|l| l.3.local == "Lange Straße"));
}

/// The label language's chain (1126): `name:<locale>`; else `name:en`
/// where `name` is in a script the locale does not read; else `name`.
#[test]
fn a_name_falls_back_to_english_where_the_locale_cannot_read_it() {
    let layers = leviculum_map::profile::layers(1, 13).expect("profile");
    fn city<'a>(x: i32, tags: &[(&'a str, &str)]) -> Tagged<'a> {
        let mut props = vec![("kind", text("locality")), ("kind_detail", text("city"))];
        props.extend(tags.iter().map(|(k, v)| (*k, text(v))));
        (POINT, at(x, 100), props)
    }
    let tile = Tile {
        layers: vec![
            layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
            tagged(
                "places",
                vec![
                    city(100, &[("name", "Αθήνα"), ("name:en", "Athens")]),
                    city(
                        200,
                        &[
                            ("name", "Москва"),
                            ("name:de", "Moskau"),
                            ("name:en", "Moscow"),
                        ],
                    ),
                    city(300, &[("name", "東京")]),
                    city(400, &[("name", "München"), ("name:en", "Munich")]),
                    city(500, &[("name", "Αθήνα"), ("name:en", "Αθήνα")]),
                ],
            ),
        ],
    }
    .encode_to_vec();
    let shown = |locale: Option<&str>| -> Vec<String> {
        let names = Names {
            locale,
            z: 13,
            deepest: false,
        };
        let (_, labels, _) = cut_tile_named(&tile, layers, Some(names)).expect("cut");
        labels
            .points
            .iter()
            .map(|p| p.3.alternate.clone().unwrap_or_else(|| p.3.local.clone()))
            .collect()
    };
    assert_eq!(
        shown(Some("de")),
        ["Athens", "Moskau", "東京", "München", "Αθήνα"]
    );
    assert_eq!(
        shown(Some("ru")),
        ["Athens", "Москва", "東京", "München", "Αθήνα"]
    );
    assert_eq!(
        shown(Some("el")),
        ["Αθήνα", "Moscow", "東京", "München", "Αθήνα"]
    );
    assert_eq!(shown(None), ["Αθήνα", "Москва", "東京", "München", "Αθήνα"]);
}

/// The region `bytes` with every tile entry's blob, by tile.
fn blobs(bytes: &[u8]) -> BTreeMap<TileId, Vec<u8>> {
    let r = Region::from_bytes(bytes).expect("open");
    (0..r.header().tiles)
        .map(|n| {
            let e = r.entry(n).expect("entry");
            let at = e.offset as usize;
            (e.id, bytes[at..at + e.len as usize].to_vec())
        })
        .collect()
}

/// Every footprint of tile `id`'s building section, rings of points.
fn footprints_of(r: &Region<&[u8]>, id: TileId) -> Vec<Vec<Vec<(i16, i16)>>> {
    let Some(b) = r.buildings(id).expect("section") else {
        return Vec::new();
    };
    b.cells()
        .flat_map(|c| c.footprints())
        .map(|f| {
            f.expect("footprint")
                .rings()
                .map(|r| r.points().collect())
                .collect()
        })
        .collect()
}

#[test]
fn the_cut_writes_the_z15_buildings_and_leaves_the_tiles_as_they_were() {
    // Without pois on either side: this test holds the version 4 rule;
    // the poi tests hold version 5's.
    let mut out = Cursor::new(Vec::new());
    let opts = CutOptions {
        pois: false,
        ..options(2)
    };
    cut(&source(false), &opts, &mut out).expect("cut");
    let with = out.into_inner();
    let mut out = Cursor::new(Vec::new());
    let opts = CutOptions {
        buildings: false,
        pois: false,
        ..options(2)
    };
    cut(&source(false), &opts, &mut out).expect("cut");
    let without = out.into_inner();

    let (rw, ro) = (
        Region::from_bytes(&with).expect("open"),
        Region::from_bytes(&without).expect("open"),
    );
    rw.verify().expect("verify");
    ro.verify().expect("verify");
    // Version 4 either way (1291); without buildings the section count is 0.
    assert_eq!(rw.header().version_of(), 4);
    assert_eq!(ro.header().version_of(), 4);
    assert_eq!(ro.building_sections(), 0);
    // The base tiles are the same bytes with and without buildings.
    assert_eq!(blobs(&with), blobs(&without));
    // One section, for the busy z15 tile with its one building; the z12
    // busy tile's building is not cut, the sea tile has none.
    assert_eq!(rw.building_sections(), 1);
    let id = TileId {
        z: 15,
        x: 17408,
        y: 11943,
    };
    assert_eq!(rw.building_entry(0).expect("entry").id, id);
    let square = vec![(100, 100), (150, 100), (150, 150), (100, 150)];
    assert_eq!(footprints_of(&rw, id), [vec![square]]);
}

/// A building the source cut at a tile edge is in each tile as that
/// tile's piece, once; a collinear outline, under a pixel, is dropped as
/// degenerate; a courtyard keeps its hole.
#[test]
fn a_building_across_a_tile_edge_is_in_each_tile_once() {
    let t = |z, x, y| TileId { z, x, y };
    let tile = |buildings: Vec<Vec<Vec<[i32; 2]>>>| -> Vec<u8> {
        Tile {
            layers: vec![
                layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
                layer(
                    "buildings",
                    buildings
                        .iter()
                        .map(|rings| (POLYGON, "building", polygon(rings)))
                        .collect(),
                ),
            ],
        }
        .encode_to_vec()
    };
    let mut hole = square(1200, 1200, 100);
    hole.reverse();
    let flat = vec![[500, 500], [600, 500], [700, 500]];
    let mut tiles = BTreeMap::new();
    // The building over the edge between x 17408 and 17409, each tile
    // holding its piece out to its buffer.
    tiles.insert(
        t(15, 17408, 11943),
        tile(vec![
            vec![square(4000, 100, 200)],
            vec![square(1100, 1100, 300), hole],
            vec![flat],
        ]),
    );
    tiles.insert(t(15, 17409, 11943), tile(vec![vec![square(-96, 100, 200)]]));
    let src = Synthetic {
        tiles,
        reversed: false,
    };
    let mut out = Cursor::new(Vec::new());
    let stats = cut(&src, &options(1), &mut out).expect("cut");
    let bytes = out.into_inner();
    let r = Region::from_bytes(&bytes).expect("open");
    r.verify().expect("verify");
    let z15 = stats.zooms[&15];
    assert_eq!(
        (
            z15.buildings_in,
            z15.buildings_out,
            z15.buildings_degenerate,
            z15.building_sections
        ),
        (4, 3, 1, 2)
    );
    let west = footprints_of(&r, t(15, 17408, 11943));
    let east = footprints_of(&r, t(15, 17409, 11943));
    let piece = |x: i16| vec![vec![(x, 100), (x + 200, 100), (x + 200, 300), (x, 300)]];
    assert_eq!(east, [piece(-96)]);
    assert_eq!(west.len(), 2);
    assert!(west.contains(&piece(4000)));
    assert!(west.iter().any(|f| f.len() == 2), "the courtyard's hole");
}

#[test]
fn the_oltrarno_fixture_cuts_its_buildings() {
    const FIXTURE: &[u8] = include_bytes!("../fixtures/oltrarno.pmtiles");
    struct InMemory;
    impl leviculum_mapcut::pmtiles::RangeRead for InMemory {
        fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
            let at = (offset as usize).min(FIXTURE.len());
            let end = at.saturating_add(len as usize).min(FIXTURE.len());
            Ok(FIXTURE[at..end].to_vec())
        }
    }
    let archive = leviculum_mapcut::pmtiles::Archive::from_source(InMemory).expect("open");
    let opts = |buildings| CutOptions {
        bounds: Bounds {
            west: 112_615_000,
            south: 437_405_000,
            east: 112_825_000,
            north: 437_545_000,
        },
        zooms: 14..=15,
        buildings,
        ..options(2)
    };
    let mut with = Cursor::new(Vec::new());
    let stats = cut(&archive, &opts(true), &mut with).expect("cut");
    let mut without = Cursor::new(Vec::new());
    cut(&archive, &opts(false), &mut without).expect("cut");
    let (with, without) = (with.into_inner(), without.into_inner());
    let r = Region::from_bytes(&with).expect("open");
    r.verify().expect("verify");
    assert_eq!(blobs(&with), blobs(&without));
    let z15 = stats.zooms[&15];
    assert_eq!(z15.building_sections, 4, "{z15:?}");
    assert!(z15.buildings_out > 100, "{z15:?}");
    assert_eq!(z15.buildings_over_device, 0);
    assert!(stats.zooms[&14].buildings_in == 0, "z14 is not cut");
    let n: usize = (0..r.building_sections())
        .map(|n| footprints_of(&r, r.building_entry(n).expect("entry").id).len())
        .sum();
    assert_eq!(n as u64, z15.buildings_out);
    println!(
        "oltrarno buildings: pieces {} footprints {} vertices {} bytes {} of region {} (without {})",
        z15.buildings_in,
        z15.buildings_out,
        z15.building_vertices,
        z15.building_bytes,
        with.len(),
        without.len()
    );
}

/// The strings of a poi, resolved: kind, name, attributes.
fn poi_strings(
    r: &Region<&[u8]>,
    s: &leviculum_map::pois::Pois<'_>,
    p: &leviculum_map::pois::Poi,
) -> (String, Option<String>, u8, Vec<(String, String)>) {
    let txt = |n: u32| r.string(n).expect("string").to_owned();
    let attrs = s.attrs_of(p).map(|(k, v)| (txt(k), txt(v))).collect();
    (txt(p.kind), p.name.map(txt), p.min_zoom, attrs)
}

/// A Protomaps 4.x tile of the travel things the cut keeps since 1685:
/// supply pois with their opening hours, the state of a way, route
/// marks, a border crossing, protected and military areas. Frankfurt
/// names on the suite's Florence tile addresses, as `named_tile` does.
fn travel_tile() -> Vec<u8> {
    let poi = |kind: &str, props: Vec<(&'static str, Value)>, x: i32| {
        let mut p = vec![("kind", text(kind)), ("min_zoom", number(14))];
        p.extend(props);
        (POINT, at(x, 200), p)
    };
    Tile {
        layers: vec![
            layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
            tagged(
                "landuse",
                vec![
                    (
                        POLYGON,
                        polygon(&[square(100, 100, 400)]),
                        vec![("kind", text("military"))],
                    ),
                    (
                        POLYGON,
                        polygon(&[square(600, 100, 400)]),
                        vec![("kind", text("nature_reserve")), ("access", text("no"))],
                    ),
                    // The control: an everyday area carries no record.
                    (
                        POLYGON,
                        polygon(&[square(1100, 100, 400)]),
                        vec![("kind", text("residential"))],
                    ),
                ],
            ),
            tagged(
                "pois",
                vec![
                    poi("drinking_water", vec![("access", text("yes"))], 300),
                    poi(
                        "spring",
                        vec![("name", text("Lohrbergquelle")), ("elevation", number(185))],
                        500,
                    ),
                    poi(
                        "fuel",
                        vec![
                            ("name", text("Tankstelle Mainkai")),
                            ("opening_hours", text("24/7")),
                        ],
                        700,
                    ),
                    poi("shelter", vec![("kind_detail", text("lean_to"))], 900),
                    poi(
                        "camp_site",
                        vec![("name", text("Campingplatz Niederrad"))],
                        1100,
                    ),
                    // A kind no code names: kept verbatim, skipped by a
                    // reader that does not know it.
                    poi("border_control", vec![], 1300),
                    // Unnamed street furniture is a poi too.
                    poi("bench", vec![], 1500),
                    // The locale's name, where it differs, as `name:alt`.
                    poi(
                        "viewpoint",
                        vec![
                            ("name", text("Belvedere")),
                            ("name:de", text("Aussichtspunkt")),
                        ],
                        1700,
                    ),
                    // In the buffer: its neighbour's.
                    (POINT, at(-20, 200), vec![("kind", text("bench"))]),
                ],
            ),
            tagged(
                "roads",
                vec![
                    (
                        LINESTRING,
                        line(&[[0, 3000], [4000, 3000]]),
                        vec![
                            ("kind", text("path")),
                            ("kind_detail", text("track")),
                            ("surface", text("gravel")),
                            ("tracktype", text("grade2")),
                            ("sac_scale", text("hiking")),
                            ("trail_visibility", text("good")),
                            ("ford", text("yes")),
                            ("access", text("private")),
                        ],
                    ),
                    (
                        LINESTRING,
                        line(&[[0, 3500], [4000, 3500]]),
                        vec![
                            ("kind", text("highway")),
                            ("name", text("A 66")),
                            ("ref", text("A 66")),
                            ("network", text("DE:BAB")),
                            ("oneway", text("yes")),
                        ],
                    ),
                ],
            ),
        ],
    }
    .encode_to_vec()
}

/// 1685: the cut keeps what OpenStreetMap carries. A cut of the
/// Frankfurt travel tile carries the supply pois with their attributes,
/// the state of the way, the route marks and the protected areas, and a
/// reader gets them back by the region's own strings.
#[test]
fn the_cut_keeps_the_travel_things_the_source_carries() {
    let mut tiles = BTreeMap::new();
    let id = TileId {
        z: 15,
        x: 17408,
        y: 11943,
    };
    tiles.insert(id, travel_tile());
    let src = Synthetic {
        tiles,
        reversed: false,
    };
    let opts = CutOptions {
        zooms: 15..=15,
        ..options(2)
    };
    let mut out = Cursor::new(Vec::new());
    let stats = cut(&src, &opts, &mut out).expect("cut");
    let bytes = out.into_inner();
    let r = Region::from_bytes(&bytes).expect("open");
    r.verify().expect("verify");
    assert_eq!(r.header().version_of(), 5);
    assert_eq!(r.poi_sections(), 1);
    let s = r.pois(id).expect("pois").expect("a section");

    // The pois: every kind kept verbatim, the buffer one left out.
    let pois: Vec<_> = s.pois().map(|p| poi_strings(&r, &s, &p)).collect();
    assert_eq!(pois.len(), 8);
    let find = |kind: &str| {
        pois.iter()
            .find(|(k, ..)| k == kind)
            .unwrap_or_else(|| panic!("{kind} missing"))
    };
    let (_, name, min_zoom, attrs) = find("drinking_water");
    assert_eq!((name.as_deref(), *min_zoom), (None, 14));
    assert_eq!(attrs, &[("access".to_owned(), "yes".to_owned())]);
    let (_, name, _, attrs) = find("spring");
    assert_eq!(name.as_deref(), Some("Lohrbergquelle"));
    assert!(attrs.contains(&("elevation".to_owned(), "185".to_owned())));
    let (_, name, _, attrs) = find("fuel");
    assert_eq!(name.as_deref(), Some("Tankstelle Mainkai"));
    assert!(attrs.contains(&("opening_hours".to_owned(), "24/7".to_owned())));
    assert!(find("shelter")
        .3
        .contains(&("kind_detail".to_owned(), "lean_to".to_owned())));
    assert_eq!(
        find("camp_site").1.as_deref(),
        Some("Campingplatz Niederrad")
    );
    find("border_control");
    assert_eq!(find("bench").1, None);
    let (_, name, _, attrs) = find("viewpoint");
    assert_eq!(name.as_deref(), Some("Belvedere"));
    assert!(attrs.contains(&("name:alt".to_owned(), "Aussichtspunkt".to_owned())));

    // The ways: the path's state, the road's route marks, the protected
    // areas' kinds; the residential area and the earth carry no record.
    let fx = r.fixture(id).expect("fixture").expect("a tile");
    let features: Vec<_> = fx.features().collect();
    let txt = |n: u32| r.string(n).expect("string").to_owned();
    let mut by_class: BTreeMap<(u8, u16), Vec<(String, String)>> = BTreeMap::new();
    for w in s.ways() {
        let f = &features[usize::from(w.feature)];
        let attrs = s.way_attrs_of(&w).map(|(k, v)| (txt(k), txt(v))).collect();
        by_class.insert((f.class.wire().0, w.feature), attrs);
    }
    assert_eq!(by_class.len(), 4, "{by_class:?}");
    let of_class = |c: Class| -> Vec<&Vec<(String, String)>> {
        by_class
            .iter()
            .filter(|((w, _), _)| *w == c.wire().0)
            .map(|(_, a)| a)
            .collect()
    };
    let path = of_class(Class::Path);
    assert_eq!(path.len(), 1);
    for (k, v) in [
        ("kind_detail", "track"),
        ("surface", "gravel"),
        ("tracktype", "grade2"),
        ("sac_scale", "hiking"),
        ("trail_visibility", "good"),
        ("ford", "yes"),
        ("access", "private"),
    ] {
        assert!(
            path[0].contains(&(k.to_owned(), v.to_owned())),
            "path lost {k}: {path:?}"
        );
    }
    let road = of_class(Class::MajorRoad);
    assert_eq!(road.len(), 1);
    for (k, v) in [("ref", "A 66"), ("network", "DE:BAB"), ("oneway", "yes")] {
        assert!(
            road[0].contains(&(k.to_owned(), v.to_owned())),
            "road lost {k}: {road:?}"
        );
    }
    let areas = of_class(Class::Urban);
    assert_eq!(areas.len(), 2, "military and nature reserve, nothing else");
    assert!(areas
        .iter()
        .any(|a| a.contains(&("kind".to_owned(), "military".to_owned()))));
    assert!(areas.iter().any(
        |a| a.contains(&("kind".to_owned(), "nature_reserve".to_owned()))
            && a.contains(&("access".to_owned(), "no".to_owned()))
    ));

    // The z15 stats carry the counts the report cites.
    let z15 = stats.zooms[&15];
    assert_eq!((z15.pois, z15.poi_ways, z15.poi_sections), (8, 4, 1));
    assert!(z15.poi_bytes > 0);
}

/// The fixture archives cut their pois: the measurement of 1685. The
/// tile blobs are the same bytes with and without them, and the numbers
/// print for the report.
#[test]
fn the_fixture_archives_measure_their_pois() {
    for (name, fixture, bounds, zooms) in [
        (
            "oltrarno",
            &include_bytes!("../fixtures/oltrarno.pmtiles")[..],
            Bounds {
                west: 112_615_000,
                south: 437_405_000,
                east: 112_825_000,
                north: 437_545_000,
            },
            14..=15,
        ),
        (
            "grunewald",
            &include_bytes!("../fixtures/grunewald.pmtiles")[..],
            Bounds {
                west: 132_250_000,
                south: 524_700_000,
                east: 132_310_000,
                north: 524_730_000,
            },
            12..=15,
        ),
    ] {
        struct InMemory(Vec<u8>);
        impl leviculum_mapcut::pmtiles::RangeRead for InMemory {
            fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
                let at = (offset as usize).min(self.0.len());
                let end = at.saturating_add(len as usize).min(self.0.len());
                Ok(self.0[at..end].to_vec())
            }
        }
        let archive = leviculum_mapcut::pmtiles::Archive::from_source(InMemory(fixture.to_vec()))
            .expect("open");
        let opts = |pois| CutOptions {
            bounds,
            zooms: zooms.clone(),
            pois,
            ..options(2)
        };
        let mut with = Cursor::new(Vec::new());
        let stats = cut(&archive, &opts(true), &mut with).expect("cut");
        let mut without = Cursor::new(Vec::new());
        cut(&archive, &opts(false), &mut without).expect("cut");
        let (with, without) = (with.into_inner(), without.into_inner());
        let r = Region::from_bytes(&with).expect("open");
        r.verify().expect("verify");
        assert_eq!(r.header().version_of(), 5);
        assert_eq!(blobs(&with), blobs(&without));
        let deepest = *zooms.end();
        let z = stats.zooms[&deepest];
        assert!(r.poi_sections() > 0, "{name}");
        assert!(z.pois > 10, "{name}: {z:?}");
        for shallow in zooms.clone().filter(|z| *z != deepest) {
            let s = stats.zooms[&shallow];
            assert_eq!((s.pois, s.poi_sections), (0, 0), "only the deepest is cut");
        }
        println!(
            "{name} pois: pois {} ways {} attrs {} sections {} poi_bytes {} \
             of region {} (without {}, labels {} buildings {})",
            z.pois,
            z.poi_ways,
            z.poi_attrs,
            z.poi_sections,
            z.poi_bytes,
            with.len(),
            without.len(),
            z.label_bytes,
            z.building_bytes,
        );
    }
}

/// A line whose vertices lie far outside the tile (1727, audit of 1707):
/// the joined-end search walks the cells of every segment's box, which
/// for a segment of 3·10^8 units each way was 5·10^12 cells, so the cut
/// never returned. Clipped to the tile and its stated window it returns
/// at once, and the line stays in the tile.
#[test]
fn a_line_reaching_far_past_the_tile_is_cut_at_once() {
    let far = 300_000_000;
    let raw = Tile {
        layers: vec![
            layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
            layer(
                "roads",
                vec![(
                    LINESTRING,
                    "major_road",
                    line(&[[-far, -far], [2048, 2048], [far, far]]),
                )],
            ),
        ],
    }
    .encode_to_vec();
    let layers = leviculum_map::profile::layers(leviculum_map::profile::CUT_1, 15)
        .expect("profile 1 at z15");
    assert!(layers.tolerance > 0, "the held simplification runs");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(cut_tile(&raw, layers).map(|(blob, st)| (blob.is_some(), st)));
    });
    let (has_blob, st) = rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("the cut returns in under one second")
        .expect("cut");
    assert!(has_blob);
    assert_eq!(st.features_out, 2, "the earth and the road: {st:?}");
}

/// One deepest zoom for the geometry, the names and the pois (1727, the
/// audit of 1707): a cut asked for a zoom deeper than the source has in
/// the box takes the deepest zoom the source has as the region's deepest
/// for all three. The geometry did so already; the names and the pois
/// took the zoom asked for, so a z14-16 cut of a z14-15 source carried no
/// poi and dropped the names only the deepest zoom carries.
#[test]
fn a_zoom_the_source_lacks_moves_no_part_of_the_deepest_cut() {
    struct InMemory(&'static [u8]);
    impl leviculum_mapcut::pmtiles::RangeRead for InMemory {
        fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
            let at = (offset as usize).min(self.0.len());
            let end = at.saturating_add(len as usize).min(self.0.len());
            Ok(self.0[at..end].to_vec())
        }
    }
    let archive = leviculum_mapcut::pmtiles::Archive::from_source(InMemory(include_bytes!(
        "../fixtures/oltrarno.pmtiles"
    )))
    .expect("open");
    let cut_to = |hi: u8| {
        let opts = CutOptions {
            bounds: Bounds {
                west: 112_615_000,
                south: 437_405_000,
                east: 112_825_000,
                north: 437_545_000,
            },
            zooms: 14..=hi,
            ..options(2)
        };
        let mut out = Cursor::new(Vec::new());
        let stats = cut(&archive, &opts, &mut out).expect("cut");
        (stats, out.into_inner())
    };
    let (have, have_bytes) = cut_to(15);
    let (asked, asked_bytes) = cut_to(16);
    assert!(
        !asked.zooms.contains_key(&16) || asked.zooms[&16].tiles == 0,
        "the source has no z16: {:?}",
        asked.zooms.get(&16)
    );
    let (h, a) = (have.zooms[&15], asked.zooms[&15]);
    assert!(h.pois > 10, "{h:?}");
    assert_eq!(
        (
            a.pois,
            a.poi_ways,
            a.point_labels,
            a.line_labels,
            a.label_bytes
        ),
        (
            h.pois,
            h.poi_ways,
            h.point_labels,
            h.line_labels,
            h.label_bytes
        ),
    );
    assert_eq!(blobs(&asked_bytes), blobs(&have_bytes));
}

/// A footprint over the device's vertex capacity at the corner-exact
/// tolerance 0 (1727, the audit of 1707: `footprints` resimplifies from
/// tolerance 1 when the base is 0, a branch no fixture reached). A round
/// building of 6,000 corners comes back simplified, kept, and within
/// [`leviculum_map::buildings::MAX_VERTICES`].
#[test]
fn a_footprint_over_the_device_capacity_is_resimplified_and_kept() {
    use leviculum_map::buildings::MAX_VERTICES;
    use leviculum_mapcut::cut::{footprints, ZoomStats, BUILDING_TOLERANCE};

    assert_eq!(BUILDING_TOLERANCE, 0);
    let n = 6000;
    let mut ring: Vec<[i32; 2]> = Vec::new();
    for i in 0..n {
        let a = std::f64::consts::TAU * f64::from(i) / f64::from(n);
        let p = [
            2048 + (1500.0 * a.cos()).round() as i32,
            2048 + (1500.0 * a.sin()).round() as i32,
        ];
        if ring.last() != Some(&p) {
            ring.push(p);
        }
    }
    assert!(ring.len() > MAX_VERTICES, "{} corners", ring.len());
    let mut st = ZoomStats::default();
    let started = std::time::Instant::now();
    let out = footprints(&[vec![ring]], &mut st);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(out.len(), 1, "kept: {st:?}");
    assert!(st.resimplified >= 1, "{st:?}");
    assert_eq!(
        (
            st.buildings_out,
            st.buildings_degenerate,
            st.buildings_over_device
        ),
        (1, 0, 0)
    );
    let corners: usize = out[0].iter().map(Vec::len).sum();
    assert!(
        (16..=MAX_VERTICES).contains(&corners),
        "{corners} corners after resimplifying"
    );
}
