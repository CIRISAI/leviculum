//! What slice (c) asked of the core (leviculum 1065): the line widths per
//! zoom band, the world outline where no region is, the region a view
//! draws when several are installed, the zooms a frontend offers, and the
//! order `Tab` steps the pins in.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_map::choice::{choose, drawn, fit_ceiling, useful_zooms, view_zooms};
use leviculum_map::draw::{draw, Ink};
use leviculum_map::mercator::{project, unproject, LatLon, World};
use leviculum_map::outline::{self, OutlineStats, BORDERS_50M, WORLD_50M};
use leviculum_map::pins::{order, Pin};
use leviculum_map::region::{Bounds, Header, Region, TileId};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{MapTheme, Profile, BACKGROUND, BORDER};

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");

const CENTRE: LatLon = LatLon {
    lat_e7: 437_750_000,
    lon_e7: 112_610_000,
};

fn region_view(zoom: u8, ink: Ink, fb: &mut [u16]) -> RenderStats {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let view = Viewport::new(project(CENTRE), zoom, 240, 320);
    let tiles = r.view(&view).expect("tiles");
    let mut edges = vec![Edge::default(); 4096];
    let mut active = vec![0u16; 4096];
    let mut heads = vec![0u16; 320];
    let mut points = vec![Point::zero(); 4096];
    let mut ring_ends = vec![0u16; 1024];
    let mut frame = Frame::new(fb, 240, 320).expect("frame");
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let Ok(st) = draw(
        &tiles,
        Profile::Full,
        ink,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    st
}

fn count(fb: &mut [u16], w: u32, h: u32, colour: Rgb565) -> u32 {
    let frame = Frame::new(fb, w, h).expect("frame");
    let mut n = 0;
    for y in 0..h {
        for x in 0..w {
            if frame.pixel(x, y) == Some(colour) {
                n += 1;
            }
        }
    }
    n
}

/// The region zooms draw thinner lines and no paths or minor roads where
/// the fixtures' widths drew a white mesh. Before 1065 the z12 view drew
/// 543 features and 35,640 stroke pixels, the z11 view 325 and 21,777;
/// before the secondary roads went a step thinner (1099), 12,885 and 9,574.
#[test]
fn region_zooms_draw_thinner_lines() {
    let mut fb = vec![0u16; 240 * 320];
    let mut rows = Vec::new();
    for zoom in [12, 11] {
        let st = region_view(zoom, Ink::Colour, &mut fb);
        assert_eq!(st.overflows, 0);
        rows.push((zoom, st.features, st.stroke.pixels));
    }
    assert_eq!(rows, [(12, 537, 11_415), (11, 325, 7_380)]);
}

/// Lines alone: no area filled, every inked pixel the one colour. Before
/// the secondary roads went a step thinner (1099): 33,138 stroked, 17,994
/// inked.
#[test]
fn the_lines_ink_fills_nothing() {
    let ink = Rgb565::new(0, 0, 0);
    let mut fb = vec![0u16; 240 * 320];
    let st = region_view(13, Ink::Lines(ink), &mut fb);
    assert_eq!(st.fill.pixels, 0);
    let (inked, bg) = (
        count(&mut fb, 240, 320, ink),
        count(&mut fb, 240, 320, BACKGROUND),
    );
    assert_eq!(inked + bg, 240 * 320, "only the ink and the background");
    assert_eq!(
        (st.features, st.stroke.pixels, inked),
        (283, 32_017, 17_634)
    );
}

#[test]
fn the_outline_table_is_natural_earth_50m() {
    let rings: Vec<_> = outline::rings(WORLD_50M).collect();
    assert_eq!(outline::bits(WORLD_50M), Some(18));
    assert_eq!(rings.len(), 1422);
    assert_eq!(rings.iter().map(|r| r.len()).sum::<usize>(), 59_239);
    assert_eq!(rings.iter().filter(|r| r.hole).count(), 1);
    assert!(WORLD_50M.len() <= 160 * 1024, "{}", WORLD_50M.len());
    // Every point decodes inside its ring's box, and on the world.
    for r in &rings {
        let [w, n, e, s] = r.bbox;
        assert!(e < 1 << 18 && s < 1 << 18, "{:?}", r.bbox);
        assert_eq!(r.points().count(), r.len());
        assert!(r
            .points()
            .all(|(x, y)| (w..=e).contains(&x) && (n..=s).contains(&y)));
    }
    // Cut short anywhere, it yields fewer rings and never panics.
    for cut in [0, 4, 5, 6, 15, 1000, WORLD_50M.len() - 1] {
        assert!(outline::rings(&WORLD_50M[..cut]).count() < 1422);
    }
}

/// The world view's layers (1683) over the land: central Europe at z2
/// draws its lakes and rivers, meets the Arctic's ice (Greenland's fills
/// at z2), and draws the disputed lines dashed beside the borders in
/// their own ink; past the outline's deepest zoom none of them is drawn,
/// and no lake lands on the water it does not own.
#[test]
fn the_world_view_draws_its_layers_over_the_land() {
    use leviculum_map::outline::WORLD;
    let view = Viewport::new(project(LatLon::from_degrees(50.0, 10.0)), 2, 240, 160);
    let mut fb = vec![0u16; 240 * 160];
    let mut frame = Frame::new(&mut fb, 240, 160).expect("frame");
    let st =
        outline::draw_world(&WORLD, &view, Ink::Colour, MapTheme::Light, &mut frame).expect("draw");
    assert!(st.overflows == 0, "{st:?}");
    assert!(st.lakes > 0 && st.lake.pixels > 0, "{st:?}");
    assert!(st.rivers > 0 && st.river.pixels > 0, "{st:?}");
    assert!(st.ice > 0, "{st:?}");
    // The glaciated areas are baked at `scalerank <= 1` (fixtures README):
    // the Alps' and Iceland's glaciers are not in the table, so the z2
    // view over Europe meets only the bounds of the Arctic's sheets and
    // fills no pixel of ice (1733: measured at z2 to z6 over the Alps,
    // Iceland and Norway). The ice sheet over Greenland is what fills.
    let icy = Viewport::new(project(LatLon::from_degrees(60.0, -45.0)), 2, 240, 160);
    let mut ifb = vec![0u16; 240 * 160];
    let mut frame = Frame::new(&mut ifb, 240, 160).expect("frame");
    let ice_st =
        outline::draw_world(&WORLD, &icy, Ink::Colour, MapTheme::Light, &mut frame).expect("draw");
    assert!(
        ice_st.ice > 0 && ice_st.snowy.pixels > 0,
        "Greenland: {ice_st:?}"
    );
    assert!(
        st.disputed > 0 && st.dash.pixels > 0,
        "the Alps' disputed lines: {st:?}"
    );
    assert!(st.borders > 0 && st.border.pixels > 0, "{st:?}");
    // Every pixel is one the layers know: water, land, a lake, ice, a
    // border line or a disputed line, which has its own ink (1733, as
    // 1683's first form of this test asked of its dashes). A colour none
    // of them paints means a layer painted outside its own.
    let water = outline::water(MapTheme::Light);
    let ice = outline::ice(MapTheme::Light);
    let disputed = outline::disputed(MapTheme::Light);
    assert!(
        ![water, ice, BACKGROUND, BORDER].contains(&disputed),
        "the disputed ink is a layer's own"
    );
    let mut known = 0;
    for colour in [water, ice, BACKGROUND, BORDER, disputed] {
        known += count(&mut fb, 240, 160, colour);
    }
    assert_eq!(known, 240 * 160, "a pixel no layer paints");
    assert!(
        count(&mut fb, 240, 160, disputed) > 0,
        "no pixel in the disputed lines' ink"
    );
    // The control: past the deepest zoom only the land is drawn.
    let deep = Viewport::new(
        project(LatLon::from_degrees(50.0, 10.0)),
        outline::MAX_ZOOM + 1,
        240,
        160,
    );
    let mut frame = Frame::new(&mut fb, 240, 160).expect("frame");
    let st =
        outline::draw_world(&WORLD, &deep, Ink::Colour, MapTheme::Light, &mut frame).expect("draw");
    assert!(
        (st.lakes, st.rivers, st.ice, st.disputed, st.borders) == (0, 0, 0, 0, 0),
        "{st:?}"
    );
}

/// The name table (1683): at z2 over Europe it holds a country's name, a
/// sea's and a range's, each inside the view with its own anchor; past
/// the outline's deepest zoom it holds none; a kind the reader does not
/// know is skipped and a table cut short anywhere ends without a panic.
#[test]
fn the_name_table_gives_the_world_views_names() {
    use leviculum_map::labels::LabelKind;
    use leviculum_map::outline::{NAMES_50M, NAMES_M9};
    let view = Viewport::new(project(LatLon::from_degrees(50.0, 10.0)), 2, 240, 160);
    let mut out = Vec::new();
    outline::names(NAMES_50M, &view, &mut out);
    assert!(out.len() > 10, "{out:?}");
    let kinds = |k: LabelKind| out.iter().filter(|l| l.kind == k).count();
    assert!(kinds(LabelKind::Country) > 0, "no country: {out:?}");
    assert!(kinds(LabelKind::Sea) > 0, "no sea: {out:?}");
    let (w, h) = view.size();
    let sub = i64::from(leviculum_mapfill::SUBPIXEL);
    for l in &out {
        assert!(
            (0..i64::from(w) * sub).contains(&l.anchor.0)
                && (0..i64::from(h) * sub).contains(&l.anchor.1),
            "{l:?} outside the view"
        );
        assert!(!l.text.is_empty());
    }
    let full = out.clone();
    let deep = Viewport::new(
        project(LatLon::from_degrees(50.0, 10.0)),
        outline::MAX_ZOOM + 1,
        240,
        160,
    );
    out.clear();
    outline::names(NAMES_50M, &deep, &mut out);
    assert!(out.is_empty(), "{out:?}");
    // The M9's cut is the same format, thinned to its zooms: at z2 it
    // carries every name the full table has in view, and no point above
    // z3.
    out.clear();
    outline::names(NAMES_M9, &view, &mut out);
    assert_eq!(out.len(), full.len(), "the M9's cut thins only past z3");
    assert!(out.iter().all(|l| l.min_zoom <= 3), "{out:?}");
    // A kind the reader does not know (1712): a point of kind 200 at the
    // view's very centre, so it is the kind and not the view that drops
    // it; the same point as a city is the positive control, exactly one
    // label; and a city after the unknown point is still read, so the
    // reader stepped over the unknown one's bytes and did not stop there.
    let centre = project(LatLon::from_degrees(50.0, 10.0));
    let (x18, y18) = (centre.x >> 14, centre.y >> 14);
    let unknown = name_table(&[(200, x18, y18, "X")]);
    out.clear();
    outline::names(&unknown, &view, &mut out);
    assert!(out.is_empty(), "the unknown kind was set: {out:?}");
    let known = name_table(&[(1, x18, y18, "X")]);
    out.clear();
    outline::names(&known, &view, &mut out);
    assert_eq!(out.len(), 1, "the control: {out:?}");
    assert_eq!((out[0].kind, out[0].text.as_str()), (LabelKind::City, "X"));
    let (w, h) = view.size();
    assert_eq!(
        out[0].anchor,
        (i64::from(w) * sub / 2, i64::from(h) * sub / 2),
        "the control lies at the view's centre"
    );
    let both = name_table(&[(200, x18, y18, "X"), (1, x18 + 4, y18, "Y")]);
    out.clear();
    outline::names(&both, &view, &mut out);
    assert_eq!(
        out.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
        ["Y"],
        "the point after the unknown one: {out:?}"
    );
    // Cut short anywhere, no name invented and never a panic: the reader
    // keeps the points it has whole and stops at the rest.
    for cut in [0, 4, 5, 6, 7, 12, NAMES_50M.len() - 1] {
        out.clear();
        outline::names(&NAMES_50M[..cut], &view, &mut out);
        assert!(out.len() <= full.len(), "{cut}: {out:?}");
    }
}

/// The texts of the world's names a view at `zoom` over the whole world
/// shows: the world at z2 is 1024 pixels square, one view takes it all.
fn world_names(zoom: u8, kind: leviculum_map::labels::LabelKind) -> Vec<String> {
    let side = 256u32 << zoom;
    let view = Viewport::new(project(LatLon::from_degrees(0.0, 0.0)), zoom, side, side);
    let mut out = Vec::new();
    outline::names(outline::NAMES_50M, &view, &mut out);
    out.into_iter()
        .filter(|l| l.kind == kind)
        .map(|l| l.text)
        .collect()
}

/// A country's name comes at the zoom Natural Earth labels it at (1712):
/// `MIN_LABEL`, floored, not `MIN_ZOOM`, which is 0 for every country. At
/// z2 the world shows Deutschland, Frankreich, Italien and Spanien and
/// not Malta, Barbados or Kap Verde, whose labels begin at z4; at z4 those
/// three are there, the control that the table has them at all.
#[test]
fn a_countrys_name_comes_at_its_label_zoom() {
    use leviculum_map::labels::LabelKind;
    let z2 = world_names(2, LabelKind::Country);
    for big in ["Deutschland", "Frankreich", "Italien", "Spanien"] {
        assert!(z2.iter().any(|t| t == big), "{big} missing at z2: {z2:?}");
    }
    let small = ["Malta", "Barbados", "Kap Verde"];
    for s in small {
        assert!(!z2.iter().any(|t| t == s), "{s} at z2: {z2:?}");
    }
    let z4 = world_names(4, LabelKind::Country);
    for s in small {
        assert!(z4.iter().any(|t| t == s), "{s} missing at z4");
    }
    // Germany, France, Russia and the United States begin at 1.7, so z1.
    let z1 = world_names(1, LabelKind::Country);
    assert!(z1.iter().any(|t| t == "Deutschland"), "{z1:?}");
    assert!(!z1.iter().any(|t| t == "Spanien"), "{z1:?}");
}

/// The M9's names table keeps every country at every zoom its overview
/// shows (1733): the board draws the world overview from z0 to the
/// outline's deepest zoom, and a cut of the table at z3 left a small
/// country such as Malta (label zoom 4) or Luxemburg (5.7) unnamed at
/// every zoom the board could show it at. At z6 on a 480 by 320 view over
/// each, the M9's table names it as a country, as the full table does.
#[test]
fn the_m9_table_names_every_country_its_overview_shows() {
    use leviculum_map::labels::LabelKind;
    for (name, lat, lon) in [("Malta", 35.9, 14.4), ("Luxemburg", 49.7, 6.1)] {
        let view = Viewport::new(project(LatLon::from_degrees(lat, lon)), 6, 480, 320);
        for (table, which) in [(outline::NAMES_M9, "M9"), (outline::NAMES_50M, "full")] {
            let mut out = Vec::new();
            outline::names(table, &view, &mut out);
            assert!(
                out.iter()
                    .any(|l| l.text == name && l.kind == LabelKind::Country),
                "{which}: no {name} at z6: {out:?}"
            );
        }
    }
}

/// The table carries each name twice where they differ (1712): the German
/// one a frontend shows (`text`) and the local one, the name in the
/// place's own country's language (`local`), not Natural Earth's English
/// `NAME`. Italy is Italien and Italia, Moscow Moskau and Москва, Milan
/// Mailand and Milano; Germany and Munich are Deutschland and München
/// both ways; a sea has no country and keeps the source's name.
#[test]
fn a_name_carries_its_german_and_its_local_form() {
    use leviculum_map::labels::LabelKind;
    let view = Viewport::new(project(LatLon::from_degrees(0.0, 0.0)), 5, 8192, 8192);
    let mut out = Vec::new();
    outline::names(outline::NAMES_50M, &view, &mut out);
    let pair = |text: &str| -> (LabelKind, String) {
        let l = out
            .iter()
            .find(|l| l.text == text)
            .unwrap_or_else(|| panic!("{text} not in the table"));
        (l.kind, l.local.clone())
    };
    assert_eq!(pair("Italien"), (LabelKind::Country, "Italia".into()));
    assert_eq!(
        pair("Deutschland"),
        (LabelKind::Country, "Deutschland".into())
    );
    assert_eq!(pair("Frankreich"), (LabelKind::Country, "France".into()));
    assert_eq!(pair("Moskau"), (LabelKind::City, "Москва".into()));
    assert_eq!(pair("Mailand").1, "Milano");
    assert_eq!(pair("München").1, "München");
    assert_eq!(pair("Wien").1, "Wien");
    // A place of a country whose language the table does not map keeps
    // the source's name as its local one: Prague is Prag and Prague.
    assert_eq!(pair("Prag").1, "Prague");
}

/// A desert is named (1712): the Sahara is a `Desert` label in the table
/// at z3 over North Africa, and the one placement rule sets it, with the
/// countries and the places around it, in boxes the size of its letters.
/// This pins behaviour that existed before order 1712: the 1683 table
/// already carried the Sahara and the rule already placed it, and the
/// test passes with 1712's non-test changes taken out (the 1733 audit's
/// mutation check). It was added as coverage that was missing, not as a
/// reproducer of a fault 1712 fixed.
#[test]
fn the_sahara_is_named_and_placed_at_z3() {
    use leviculum_map::labels::{place, LabelKind, Rect, Rule};
    let view = Viewport::new(project(LatLon::from_degrees(22.0, 10.0)), 3, 480, 320);
    let mut out = Vec::new();
    outline::names(outline::NAMES_50M, &view, &mut out);
    let sahara: Vec<_> = out.iter().filter(|l| l.kind == LabelKind::Desert).collect();
    assert!(
        sahara.iter().any(|l| l.text == "Sahara"),
        "no Sahara at z3: {out:?}"
    );
    let candidates = boxed(&out);
    let rule = Rule {
        margin: 3,
        block: (0, 0),
        repeat: 300,
    };
    let placed = place(&candidates, &[], rule);
    let set: Vec<&str> = placed
        .iter()
        .map(|&i| candidates[i].text.as_str())
        .collect();
    assert!(set.contains(&"Sahara"), "the Sahara not placed: {set:?}");
    // The control: a pin's box over the Sahara's own takes its place.
    let i = candidates
        .iter()
        .position(|c| c.text == "Sahara")
        .expect("Sahara");
    let pin: Vec<Rect> = candidates[i].boxes.clone();
    let set: Vec<&str> = place(&candidates, &pin, rule)
        .iter()
        .map(|&i| candidates[i].text.as_str())
        .collect();
    assert!(!set.contains(&"Sahara"), "{set:?}");
}

/// `labels` as candidates in pixels: a box 7 pixels a letter wide and 14
/// tall, centred on the anchor, as a window's font sets a name.
fn boxed(labels: &[leviculum_map::labels::ViewLabel]) -> Vec<leviculum_map::labels::Candidate> {
    use leviculum_map::labels::{Candidate, Rect};
    let sub = i64::from(leviculum_mapfill::SUBPIXEL);
    labels
        .iter()
        .map(|l| {
            let (x, y) = ((l.anchor.0 / sub) as i32, (l.anchor.1 / sub) as i32);
            let half = 7 * l.text.chars().count() as i32 / 2;
            Candidate {
                kind: l.kind,
                rank: l.rank,
                text: l.text.clone(),
                at: l.at,
                boxes: vec![Rect {
                    x0: x - half,
                    y0: y - 7,
                    x1: x + half,
                    y1: y + 7,
                }],
            }
        })
        .collect()
}

/// A country's name is served before a city's (1712, the decision
/// `labels::LabelKind::priority` documents): over the world, where only
/// the world table brings countries, a country is the larger fact, and a
/// city that stands on its box gives way. The pure case, through the one
/// placement rule: one box, a country and a city, and behind the places a
/// country before a sea. What a frontend sets over the world at z2 is not
/// asserted here (1733): that depends on its own letter metrics, which
/// this crate does not have, so the window's test
/// (`loquax::gui::map`, `the_world_view_names_its_countries_and_seas_at_z2_and_z6`)
/// asserts it with `loquax::mapnames`' typesetter: Deutschland and Italien
/// set, Paris not. Italien is checked there.
#[test]
fn a_country_is_placed_before_a_city_that_covers_it() {
    use leviculum_map::labels::{place, Candidate, LabelKind, Rect, Rule};
    let rule = Rule {
        margin: 3,
        block: (0, 0),
        repeat: 300,
    };
    let b = Rect {
        x0: 0,
        y0: 0,
        x1: 40,
        y1: 14,
    };
    let one = |kind, text: &str, rank| Candidate {
        kind,
        rank,
        text: text.into(),
        at: (0, 0),
        boxes: vec![b],
    };
    // The city first in the list and of the higher rank: neither decides.
    let both = [
        one(LabelKind::City, "Paris", 7),
        one(LabelKind::Country, "Frankreich", 0),
    ];
    assert_eq!(place(&both, &[], rule), [1]);
    // Behind the places a country still comes before a sea.
    let sea = [
        one(LabelKind::Sea, "Nordsee", 7),
        one(LabelKind::Country, "Frankreich", 0),
    ];
    assert_eq!(place(&sea, &[], rule), [1]);
}

/// A name table of `points` (kind, x, y at 18 bits, text), each with its
/// own string and no alternate, in the bake's format (`LWN1`).
fn name_table(points: &[(u8, u32, u32, &str)]) -> Vec<u8> {
    fn uv(out: &mut Vec<u8>, mut v: u32) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    let mut t = b"LWN1".to_vec();
    t.push(18);
    uv(&mut t, points.len() as u32);
    let mut at = 0u32;
    t.extend_from_slice(&at.to_le_bytes());
    for p in points {
        at += p.3.len() as u32;
        t.extend_from_slice(&at.to_le_bytes());
    }
    for p in points {
        t.extend_from_slice(p.3.as_bytes());
    }
    uv(&mut t, points.len() as u32);
    for (i, &(kind, x, y, _)) in points.iter().enumerate() {
        t.extend_from_slice(&[kind, 0, 0, 0]);
        uv(&mut t, x);
        uv(&mut t, y);
        uv(&mut t, i as u32);
        uv(&mut t, 0);
    }
    t
}

/// The 1:50m land boundary lines: every line decodes inside its box, the
/// ring reader sees none of them, and a table cut short anywhere yields
/// fewer lines and never panics.
#[test]
fn the_border_table_is_natural_earth_50m() {
    let lines: Vec<_> = outline::lines(BORDERS_50M).collect();
    assert_eq!(outline::bits(BORDERS_50M), Some(18));
    assert_eq!(lines.len(), 393);
    assert_eq!(lines.iter().map(|l| l.len()).sum::<usize>(), 19_760);
    assert!(lines.iter().all(|l| !l.hole && l.len() >= 2));
    assert_eq!(BORDERS_50M.len(), 49_512);
    assert!(BORDERS_50M.len() <= 100 * 1024);
    for l in &lines {
        let [w, n, e, s] = l.bbox;
        assert!(e < 1 << 18 && s < 1 << 18, "{:?}", l.bbox);
        assert_eq!(l.points().count(), l.len());
        assert!(l
            .points()
            .all(|(x, y)| (w..=e).contains(&x) && (n..=s).contains(&y)));
    }
    assert_eq!(outline::rings(BORDERS_50M).count(), 0);
    assert_eq!(outline::lines(WORLD_50M).count(), 0);
    for cut in [0, 4, 5, 6, 15, 1000, BORDERS_50M.len() - 1] {
        assert!(outline::lines(&BORDERS_50M[..cut]).count() < 393);
    }
}

fn outline_view(view: &Viewport, ink: Ink, fb: &mut [u16]) -> OutlineStats {
    let (w, h) = view.size();
    let mut frame = Frame::new(fb, w, h).expect("frame");
    let Ok(st) = outline::draw(
        WORLD_50M,
        BORDERS_50M,
        view,
        ink,
        MapTheme::Light,
        &mut frame,
    );
    st
}

fn land_view(view: &Viewport, ink: Ink, fb: &mut [u16]) -> OutlineStats {
    let (w, h) = view.size();
    let mut frame = Frame::new(fb, w, h).expect("frame");
    let Ok(st) = outline::draw(WORLD_50M, &[], view, ink, MapTheme::Light, &mut frame);
    st
}

/// The world at z2 with no region: land in the earth colour over water,
/// and the same view as coasts alone; then both again with the borders.
#[test]
fn the_outline_draws_the_world_at_z2() {
    let view = Viewport::new(project(LatLon::from_degrees(20.0, 10.0)), 2, 240, 160);
    let mut fb = vec![0u16; 240 * 160];
    let st = land_view(&view, Ink::Colour, &mut fb);
    assert_eq!(st.overflows, 0);
    let land = count(&mut fb, 240, 160, BACKGROUND);
    let water = count(&mut fb, 240, 160, outline::water(MapTheme::Light));
    assert_eq!(land + water, 240 * 160);
    assert_eq!(
        (st.rings, st.culled, st.vertices, st.fill.pixels, land),
        (122, 1_300, 4_695, 22_074, 22_074)
    );
    assert_eq!((st.borders, st.border.pixels), (0, 0));
    let st = outline_view(&view, Ink::Colour, &mut fb);
    let land = count(&mut fb, 240, 160, BACKGROUND);
    let water = count(&mut fb, 240, 160, outline::water(MapTheme::Light));
    let border = count(&mut fb, 240, 160, BORDER);
    assert_eq!(land + water + border, 240 * 160);
    assert_eq!(
        (
            st.rings,
            st.fill.pixels,
            st.borders,
            st.border.pixels,
            land,
            border
        ),
        (122, 22_074, 166, 2_521, 19_819, 2_292)
    );
    let ink = Rgb565::new(0, 0, 0);
    let st = land_view(&view, Ink::Lines(ink), &mut fb);
    assert_eq!(st.fill.pixels, 0);
    let coast = count(&mut fb, 240, 160, ink);
    assert_eq!((st.rings, st.stroke.pixels, coast), (122, 1_931, 1_812));
    let st = outline_view(&view, Ink::Lines(ink), &mut fb);
    let inked = count(&mut fb, 240, 160, ink);
    assert_eq!(
        (st.stroke.pixels, st.borders, st.border.pixels, inked),
        (1_931, 166, 2_521, 3_997)
    );
}

/// Central Europe at z5 with no region, where the borders are dense: they
/// lie over the land in the border colour, and as dots of the ink when the
/// coasts are lines alone.
#[test]
fn the_outline_draws_borders_at_z5() {
    let view = Viewport::new(project(LatLon::from_degrees(48.0, 12.0)), 5, 240, 160);
    let mut fb = vec![0u16; 240 * 160];
    let st = land_view(&view, Ink::Colour, &mut fb);
    let bare = count(&mut fb, 240, 160, BACKGROUND);
    assert_eq!((st.rings, st.fill.pixels, bare), (1, 38_325, 38_325));
    let st = outline_view(&view, Ink::Colour, &mut fb);
    assert_eq!(st.overflows, 0);
    let land = count(&mut fb, 240, 160, BACKGROUND);
    let water = count(&mut fb, 240, 160, outline::water(MapTheme::Light));
    let border = count(&mut fb, 240, 160, BORDER);
    assert_eq!(land + water + border, 240 * 160);
    assert_eq!(bare, land + border, "the borders lie on the land");
    assert_eq!(
        (
            st.rings,
            st.fill.pixels,
            st.borders,
            st.border.pixels,
            land,
            border
        ),
        (1, 38_325, 23, 1_181, 37_167, 1_158)
    );
    let ink = Rgb565::new(0, 0, 0);
    let st = land_view(&view, Ink::Lines(ink), &mut fb);
    let coast = count(&mut fb, 240, 160, ink);
    let st2 = outline_view(&view, Ink::Lines(ink), &mut fb);
    let inked = count(&mut fb, 240, 160, ink);
    assert_eq!(
        (st.stroke.pixels, coast, st2.border.pixels, inked),
        (27, 27, 1_181, 1_185)
    );
}

/// The borders stop at the outline's deepest zoom: the Alps where Austria,
/// Switzerland and Liechtenstein meet have borders at z10 and none at z11.
#[test]
fn the_borders_stop_below_max_zoom() {
    let at = project(LatLon::from_degrees(47.1, 9.55));
    let mut fb = vec![0u16; 100 * 100];
    let st = outline_view(
        &Viewport::new(at, outline::MAX_ZOOM, 100, 100),
        Ink::Colour,
        &mut fb,
    );
    assert!(st.borders > 0 && st.border.pixels > 0, "{st:?}");
    assert!(count(&mut fb, 100, 100, BORDER) > 0);
    let st = outline_view(
        &Viewport::new(at, outline::MAX_ZOOM + 1, 100, 100),
        Ink::Colour,
        &mut fb,
    );
    assert_eq!((st.borders, st.border.pixels), (0, 0));
    assert_eq!(count(&mut fb, 100, 100, BORDER), 0);
}

/// Italy at z6, the zoom a reader passes on the way from the world to a
/// region: the coast and its islands, filled and stroked, and the Vatican's
/// border, the one line in view.
#[test]
fn the_outline_draws_italy_at_z6() {
    let view = Viewport::new(project(LatLon::from_degrees(42.0, 12.5)), 6, 240, 160);
    let mut fb = vec![0u16; 240 * 160];
    let st = land_view(&view, Ink::Colour, &mut fb);
    assert_eq!(st.overflows, 0);
    let land = count(&mut fb, 240, 160, BACKGROUND);
    assert_eq!(
        (st.rings, st.culled, st.vertices, st.fill.pixels, land),
        (4, 1_418, 87, 18_549, 18_549)
    );
    let st = outline_view(&view, Ink::Colour, &mut fb);
    let land = count(&mut fb, 240, 160, BACKGROUND);
    let border = count(&mut fb, 240, 160, BORDER);
    assert_eq!(
        (st.borders, st.border.pixels, land, border),
        (1, 4, 18_545, 4)
    );
    let ink = Rgb565::new(0, 0, 0);
    let st = land_view(&view, Ink::Lines(ink), &mut fb);
    let coast = count(&mut fb, 240, 160, ink);
    assert_eq!((st.rings, st.stroke.pixels, coast), (4, 409, 409));
}

/// Across the antimeridian the world repeats: a view centred on 180 E draws
/// the land of both sides, and at z0 a view wider than the world (256
/// pixels) draws two copies of it. Every point stays inside the filler's limit at the deepest zoom.
#[test]
fn the_outline_wraps_and_holds_at_every_zoom() {
    let mut fb = vec![0u16; 400 * 100];
    let wide = Viewport::new(World { x: 0, y: 1 << 31 }, 0, 400, 100);
    let st = outline_view(&wide, Ink::Colour, &mut fb);
    assert_eq!(st.rings + st.culled, 2 * 1422, "two copies of the world");
    assert!(st.borders > 393, "borders of both copies: {}", st.borders);
    let land = count(&mut fb, 400, 100, BACKGROUND);
    assert!(land > 0);
    for zoom in [8, 14, 20] {
        let view = Viewport::new(project(CENTRE), zoom, 100, 100);
        let mut fb = vec![0u16; 100 * 100];
        let st = outline_view(&view, Ink::Colour, &mut fb);
        assert_eq!(st.overflows, 0, "z{zoom}");
        // Florence is on land at every zoom.
        assert_eq!(count(&mut fb, 100, 100, BACKGROUND), 100 * 100, "z{zoom}");
    }
}

fn header(min: u8, max: u8, bounds: Bounds) -> Header {
    let mut h = *Region::from_bytes(FLORENCE).expect("open").header();
    h.min_zoom = min;
    h.max_zoom = max;
    h.bounds = bounds;
    h
}

/// Two overlapping regions: the deeper wins where both cover the centre,
/// a tie goes to the first file name, and outside both, or above both
/// zoom ranges, the view draws the outline.
#[test]
fn the_region_with_the_deepest_zoom_at_the_centre_is_drawn() {
    let florence = *Region::from_bytes(FLORENCE).expect("open").header();
    let tuscany = header(
        7,
        13,
        Bounds {
            west: 96_000_000,
            south: 424_000_000,
            east: 124_000_000,
            north: 442_000_000,
        },
    );
    let at = project(CENTRE);
    let siena = project(LatLon::from_degrees(43.318, 11.331));
    let both = [("tuscany.lmr", &tuscany), ("firenze.lmr", &florence)];
    assert_eq!(choose(both, at, 13), Some(1));
    assert_eq!(choose(both, siena, 13), Some(0), "only Tuscany holds Siena");
    // Florence starts at z11; Tuscany draws z9.
    assert_eq!(choose(both, at, 9), Some(0));
    assert_eq!(choose(both, at, 6), None, "above both: the outline");
    assert_eq!(
        choose(both, project(LatLon::from_degrees(0.0, 0.0)), 12),
        None
    );
    // Equal max zoom: the first name.
    let twin = header(11, 15, florence.bounds);
    assert_eq!(
        choose([("b.lmr", &florence), ("a.lmr", &twin)], at, 13),
        Some(1)
    );
    assert_eq!(
        choose([("a.lmr", &florence), ("b.lmr", &twin)], at, 13),
        Some(0)
    );
}

/// A fit goes no deeper than the deepest region whose box holds its
/// centre has data for, its max zoom without the overzoom; the outline's
/// deepest where no box does (1074).
#[test]
fn a_fit_goes_no_deeper_than_the_region_under_it_has_data() {
    let florence = *Region::from_bytes(FLORENCE).expect("open").header();
    let tuscany = header(
        7,
        13,
        Bounds {
            west: 96_000_000,
            south: 424_000_000,
            east: 124_000_000,
            north: 442_000_000,
        },
    );
    let at = project(CENTRE);
    let siena = project(LatLon::from_degrees(43.318, 11.331));
    let both = [&tuscany, &florence];
    assert_eq!(fit_ceiling(both, at), florence.max_zoom);
    assert_eq!(fit_ceiling(both, siena), tuscany.max_zoom);
    let nowhere = project(LatLon::from_degrees(0.0, 0.0));
    assert_eq!(fit_ceiling(both, nowhere), outline::MAX_ZOOM);
    assert_eq!(fit_ceiling([], at), outline::MAX_ZOOM);
}

/// A box across the antimeridian holds both sides of it.
#[test]
fn a_box_across_the_antimeridian_holds_both_sides() {
    let fiji = Bounds {
        west: 1_770_000_000,
        south: -190_000_000,
        east: -1_790_000_000,
        north: -160_000_000,
    };
    assert!(fiji.contains(LatLon::from_degrees(-17.0, 178.0)));
    assert!(fiji.contains(LatLon::from_degrees(-17.0, -179.5)));
    assert!(!fiji.contains(LatLon::from_degrees(-17.0, 0.0)));
}

#[test]
fn the_zoom_range_reaches_from_the_world_to_the_deepest_overzoom() {
    let florence = *Region::from_bytes(FLORENCE).expect("open").header();
    assert_eq!(useful_zooms(&florence), 11..=20);
    let shallow = header(0, 6, florence.bounds);
    assert_eq!(useful_zooms(&shallow), 0..=14);
    assert_eq!(view_zooms([]), 0..=outline::MAX_ZOOM);
    assert_eq!(view_zooms([&shallow]), 0..=14);
    assert_eq!(view_zooms([&shallow, &florence]), 0..=20);
}

/// `Tab` goes by name, case folded, and equal names by hash.
#[test]
fn pins_are_stepped_by_name_then_hash() {
    let at = World { x: 0, y: 0 };
    let pin = |label| Pin {
        at,
        label,
        stale: false,
    };
    let pins = [pin("carol"), pin("Bob"), pin("alice"), pin("bob")];
    let hashes = [[3u8; 16], [9; 16], [1; 16], [2; 16]];
    assert_eq!(order(&pins, |i| hashes[i]), [2, 3, 1, 0]);
}

/// The wheel zooms about the pointer: the world point at the pixel under
/// it stays at that pixel, in and out, in the middle and at a corner.
#[test]
fn a_zoom_about_a_pixel_keeps_the_point_under_it() {
    for (x, y) in [(10, 20), (239, 0), (120, 80), (0, 159)] {
        for (from, to) in [(6, 7), (7, 6), (12, 14), (14, 3)] {
            let mut view = Viewport::new(project(CENTRE), from, 240, 160);
            let held = view.to_world(x, y);
            view.zoom_about(to, x, y);
            assert_eq!(view.zoom(), to);
            assert_eq!(view.to_world(x, y), held, "z{from} to z{to} at ({x}, {y})");
        }
    }
}

/// A box around a place (loquax's autoload, 1089): as many metres each
/// way north and south as east and west, held by a box around it and not
/// by a smaller one, wrapped at the antimeridian.
#[test]
fn a_box_around_a_place_and_one_box_in_another() {
    let frankfurt = LatLon::from_degrees(50.1109, 8.6821);
    let b = Bounds::around(frankfurt, 6_000);
    assert!(b.contains(frankfurt));
    // 12 km is 0.1078 degrees of latitude, and at 50.11 N 0.1681 of longitude.
    assert!((b.north - b.south - 1_078_000).abs() < 1_000, "{b:?}");
    assert!((b.east - b.west - 1_681_000).abs() < 2_000, "{b:?}");
    let inner = Bounds::around(frankfurt, 3_000);
    assert!(b.holds(&inner) && !inner.holds(&b) && b.holds(&b));
    let fiji = Bounds::around(LatLon::from_degrees(-17.0, 179.99), 6_000);
    assert!(fiji.west > fiji.east, "across the antimeridian: {fiji:?}");
    assert!(fiji.holds(&fiji) && !fiji.holds(&inner));
}

/// A view's box is its corners, and the whole world's longitudes once the
/// view is wider than the world.
#[test]
fn a_views_box_is_its_corners() {
    let view = Viewport::new(project(LatLon::from_degrees(50.1109, 8.6821)), 13, 400, 200);
    let b = view.bounds();
    let nw = unproject(view.to_world(0, 0));
    let se = unproject(view.to_world(400, 200));
    assert_eq!(
        (b.west, b.north, b.east, b.south),
        (nw.lon_e7, nw.lat_e7, se.lon_e7, se.lat_e7)
    );
    assert!(b.contains(LatLon::from_degrees(50.1109, 8.6821)));
    let world = Viewport::new(project(LatLon::from_degrees(0.0, 0.0)), 0, 512, 256).bounds();
    assert_eq!((world.west, world.east), (-1_800_000_000, 1_800_000_000));
}

/// Every region that reaches into a view is drawn, the deepest last; one
/// outside the view or past its zooms is not.
#[test]
fn every_region_in_a_view_is_drawn_the_deepest_last() {
    let header = |west: i32, east: i32, max_zoom: u8| Header {
        bounds: Bounds {
            west,
            south: 500_000_000,
            east,
            north: 502_000_000,
        },
        min_zoom: 10,
        max_zoom,
        profile: 1,
        attribution: (0, 0),
        notice: (0, 0),
        tiles: 0,
        index: 0,
        len: 0,
        labels: None,
        buildings: None,
        pois: None,
        streamed: false,
    };
    let regions = [
        ("a.lmr", header(86_000_000, 86_800_000, 15)),
        ("b.lmr", header(86_800_000, 87_600_000, 14)),
        ("far.lmr", header(100_000_000, 101_000_000, 15)),
        (
            "deep.lmr",
            Header {
                min_zoom: 14,
                ..header(86_000_000, 87_600_000, 15)
            },
        ),
    ];
    let view = Viewport::new(project(LatLon::from_degrees(50.1, 8.68)), 12, 400, 200);
    let order = drawn(regions.iter().map(|(n, h)| (*n, h)), &view);
    assert_eq!(
        order,
        [1, 0],
        "b (z14) under a (z15); far, and deep from z14 on, left out"
    );
}

/// A tile not arrived yet is stood in for by a coarser one, placed so that
/// its part over the tile lands where the tile would (1291).
#[test]
fn a_coarser_tile_stands_in_where_the_tile_goes() {
    use leviculum_map::viewport::{ViewTile, MAX_OVERZOOM};
    let tile = ViewTile {
        id: TileId { z: 15, x: 5, y: 7 },
        overzoom: 1,
        origin: Point::new(100, 200),
    };
    let up = tile.up(1).expect("one up");
    assert_eq!(up.id, TileId { z: 14, x: 2, y: 3 });
    assert_eq!(up.overzoom, 2);
    // The child is the lower right quarter: its parent starts one child
    // (8192 sub-pixels at overzoom 1) up and left.
    assert_eq!(up.origin, Point::new(100 - 8192, 200 - 8192));
    let two = tile.up(2).expect("two up");
    assert_eq!(two.id, TileId { z: 13, x: 1, y: 1 });
    assert_eq!(two.origin, Point::new(100 - 8192, 200 - 3 * 8192));
    assert_eq!(
        tile.up(MAX_OVERZOOM),
        None,
        "past the overzoom the filler takes"
    );
    assert_eq!(tile.up(16), None, "above z0");
}
