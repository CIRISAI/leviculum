//! LMR2: the names a region carries. A label section and a string table
//! round trip through their writer and reader; the committed Florence
//! region (cut with `--locale de`) holds the places and street names its
//! tiles name; a version 1 file still reads, without names; and a version
//! this reader does not know is refused, never misread. A street's pieces
//! are joined end to end before its name is placed (1126).

use leviculum_map::labels::{
    encode_section, encode_strings, join_pieces, place, Candidate, LabelError, LabelKind,
    LineLabel, PointLabel, Rect, Rule, Section, ViewLabel, POINT_LEN, SECTION_HEAD,
};
use leviculum_map::region::{
    check_strings, Entry, Header, Region, RegionError, TileId, ENTRY_LEN, HEADER_LEN,
};
use leviculum_mapfill::fixture::Kind;

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");
/// The same region as the cut wrote it before 1291: a frozen version 2
/// file, the sample the old-version reading tests hold to (1707 re-cut
/// the live fixtures, which are version 4 now).
const OLD_V2: &[u8] = include_bytes!("../fixtures/old/firenze-v2.lmr");

fn point(kind: LabelKind, string: u32, x: i16, y: i16) -> PointLabel {
    PointLabel {
        kind,
        rank: 7,
        min_zoom: 11,
        flags: 0,
        string,
        x,
        y,
    }
}

#[test]
fn a_label_section_round_trips() {
    let points = [
        point(LabelKind::City, 3, 0, 4095),
        point(LabelKind::Water, 1, 2048, 17),
    ];
    let lines = [LineLabel {
        feature: 41,
        kind: LabelKind::MinorRoad,
        min_zoom: 15,
        string: 0,
    }];
    // Given out of order; the writer sorts them.
    let alts = [(2, 4), (0, 2)];
    let bytes = encode_section(&points, &lines, &alts);
    assert_eq!(bytes.len(), SECTION_HEAD + 2 * POINT_LEN + 8 + 2 * 8);
    let s = Section::parse(&bytes, 5).expect("parse");
    let got: Vec<PointLabel> = s.points().map(|(_, p)| p).collect();
    assert_eq!(got, points);
    let got: Vec<(u16, LineLabel)> = s.lines().collect();
    assert_eq!(got, [(2, lines[0])]);
    assert_eq!(s.alternates().collect::<Vec<_>>(), [(0, 2), (2, 4)]);
    assert_eq!(
        (s.alternate(0), s.alternate(1), s.alternate(2)),
        (Some(2), None, Some(4))
    );

    // A string past the table, an anchor outside the tile, a reserved
    // byte, a length that does not match: each an error.
    assert_eq!(Section::parse(&bytes, 4).err(), Some(LabelError::String(4)));
    let mut b = bytes.clone();
    b[SECTION_HEAD + 8..SECTION_HEAD + 10].copy_from_slice(&4096i16.to_le_bytes());
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Anchor(0)));
    let mut b = bytes.clone();
    b[6] = 1;
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Reserved));
    let mut b = bytes.clone();
    b.push(0);
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Length));
    // Alternates out of order, or naming a label that is not there.
    let mut b = bytes.clone();
    let alt = bytes.len() - 16;
    b[alt..alt + 2].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Alternate(2)));

    // A kind this reader does not know is skipped, not refused: a later
    // cut may add one.
    let mut b = bytes.clone();
    b[SECTION_HEAD] = 200;
    let s = Section::parse(&b, 5).expect("an unknown kind parses");
    assert_eq!(s.points().map(|(n, _)| n).collect::<Vec<_>>(), [1]);
    assert_eq!(s.point_count(), 2);
}

#[test]
fn a_string_table_is_sorted_and_searched() {
    let (count, table) = encode_strings(["Arno", "Firenze", "Florenz", "Via dei Servi"]);
    assert_eq!(count, 4);
    check_strings(&table, count).expect("table");
    let (_, unsorted) = encode_strings(["Firenze", "Arno"]);
    assert_eq!(check_strings(&unsorted, 2), Err(RegionError::Strings(1)));
    let (_, twice) = encode_strings(["Arno", "Arno"]);
    assert_eq!(check_strings(&twice, 2), Err(RegionError::Strings(1)));
    // Cut one byte short: the last string runs past the bytes.
    assert_eq!(
        check_strings(&table[..table.len() - 1], 4),
        Err(RegionError::Strings(3))
    );

    let r = Region::from_bytes(FLORENCE).expect("open");
    let n = r.string_count();
    assert!(n > 200, "{n} strings");
    let firenze = r.find_string("Firenze").expect("search").expect("Firenze");
    assert_eq!(r.string(firenze), Ok("Firenze"));
    assert_eq!(r.find_string("Frankfurt am Main").expect("search"), None);
    let mut s = String::new();
    for i in [0, firenze, n - 1] {
        r.read_string(i, &mut s).expect("read");
        assert_eq!(Ok(s.as_str()), r.string(i));
    }
    assert_eq!(r.string(n), Err(RegionError::Strings(n)));
}

#[test]
fn the_florence_region_names_its_places_and_streets() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    r.verify().expect("verify");
    let l = r.header().labels.expect("a version 2 file");
    assert_eq!(l.sections, 6, "every tile of the fixture has names");

    // z11: Florence, its towns and villages, and the German name where it
    // differs; no locality, hamlet or neighbourhood at a zoom that has
    // none of them.
    let z11 = TileId {
        z: 11,
        x: 1088,
        y: 746,
    };
    let s = r.labels(z11).expect("labels").expect("a section");
    let named: Vec<(LabelKind, &str, Option<&str>)> = s
        .points()
        .map(|(n, p)| {
            let alt = s.alternate(n).map(|a| r.string(a).expect("alt"));
            (p.kind, r.string(p.string).expect("name"), alt)
        })
        .collect();
    assert_eq!(
        named[0],
        (LabelKind::City, "Firenze", Some("Florenz")),
        "{named:?}"
    );
    assert!(named.iter().all(|(k, ..)| k.is_place()), "{named:?}");
    assert!(named
        .iter()
        .any(|(k, n, _)| *k == LabelKind::Town && *n == "Fiesole"));
    // Rivers are named from their own min_zoom (1685): the Arno and its
    // brooks are line labels here; streets are not.
    let lines: Vec<LabelKind> = s.lines().map(|(_, l)| l.kind).collect();
    assert!(!lines.is_empty(), "the Arno is named at z11");
    assert!(
        lines.iter().all(|k| !k.is_road()),
        "no street names below z14: {lines:?}"
    );

    // z15: street names, each a line feature of its own tile.
    let z15 = TileId {
        z: 15,
        x: 17408,
        y: 11943,
    };
    let s = r.labels(z15).expect("labels").expect("a section");
    let fx = r.fixture(z15).expect("tile").expect("geometry");
    let features: Vec<Kind> = fx.features().map(|f| f.kind).collect();
    let streets: Vec<&str> = s
        .lines()
        .map(|(_, line)| {
            assert_eq!(features[usize::from(line.feature)], Kind::Line);
            assert!(line.kind.is_road() && line.min_zoom >= 14);
            r.string(line.string).expect("name")
        })
        .collect();
    assert!(streets.contains(&"Via del Proconsolo"), "{streets:?}");
}

/// The Florence region written as version 1: the same tiles under a
/// 64-byte header, no names.
fn as_version_1(v2: &Region<&[u8]>) -> Vec<u8> {
    let h = *v2.header();
    let mut out = vec![0u8; HEADER_LEN];
    let text = |s: &str, out: &mut Vec<u8>| {
        let at = out.len() as u32;
        out.extend_from_slice(s.as_bytes());
        (at, s.len() as u32)
    };
    let attribution = text(v2.attribution(), &mut out);
    let notice = text(v2.notice(), &mut out);
    let mut entries = Vec::new();
    let mut buf = Vec::new();
    for n in 0..h.tiles {
        let e = v2.entry(n).expect("entry");
        v2.read_tile(&e, &mut buf).expect("tile");
        entries.push(Entry {
            offset: out.len() as u64,
            ..e
        });
        out.extend_from_slice(&buf);
    }
    let index = out.len() as u64;
    for e in &entries {
        out.extend_from_slice(&e.encode());
    }
    let header = Header {
        attribution,
        notice,
        index,
        len: out.len() as u64,
        labels: None,
        buildings: None,
        ..h
    };
    out[..HEADER_LEN].copy_from_slice(&header.encode());
    out
}

#[test]
fn a_version_1_file_still_reads_without_names() {
    let v2 = Region::from_bytes(OLD_V2).expect("open");
    let bytes = as_version_1(&v2);
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 1);
    let v1 = Region::from_bytes(&bytes).expect("a version 1 file opens");
    v1.verify().expect("verify");
    assert_eq!(v1.header().labels, None);
    assert_eq!(v1.string_count(), 0);
    for n in 0..v2.header().tiles {
        let id = v2.entry(n).expect("entry").id;
        assert!(v1.labels(id).expect("labels").is_none());
        let (a, b) = (v1.fixture(id).expect("v1"), v2.fixture(id).expect("v2"));
        let count = |f: Option<leviculum_mapfill::fixture::Fixture<'_>>| f.map(|f| f.len());
        assert_eq!(count(a), count(b), "tile {id:?}");
    }
    assert_eq!(v1.find_string("Firenze"), Ok(None));
}

#[test]
fn a_version_this_reader_does_not_know_is_refused() {
    let mut b = OLD_V2.to_vec();
    b[4] = 6;
    assert_eq!(Region::from_bytes(&b).err(), Some(RegionError::Version(6)));
    // A version 1 reader's check is the same field: it sees 2 and stops,
    // where reading on would take the label index for tiles. Here a v2
    // file claiming version 1, 3, 4 or 5 is caught by its header length.
    for v in [1, 3, 4, 5] {
        let mut b = OLD_V2.to_vec();
        b[4] = v;
        assert_eq!(
            Region::from_bytes(&b).err(),
            Some(RegionError::Header("header length")),
            "version {v}"
        );
    }
    // The label index must end where the tile index starts.
    let r = Region::from_bytes(OLD_V2).expect("open");
    let mut h = *r.header();
    let mut b = OLD_V2.to_vec();
    if let Some(l) = h.labels.as_mut() {
        l.index -= ENTRY_LEN as u64;
    }
    b[..h.header_len()].copy_from_slice(&h.encode());
    assert_eq!(
        Region::from_bytes(&b).err(),
        Some(RegionError::Header("label index"))
    );
}

fn candidate(kind: LabelKind, rank: u8, text: &str, x: i32, y: i32, len: i32) -> Candidate {
    Candidate {
        kind,
        rank,
        text: text.to_owned(),
        at: (x as u32, y as u32),
        boxes: vec![Rect {
            x0: x,
            y0: y,
            x1: x + len,
            y1: y + 1,
        }],
    }
}

/// The one rule: pins first and never covered, then places by class and
/// rank, waters and parks, streets; greedy and in an order that does not
/// depend on the order the frontend handed the labels in.
#[test]
fn placement_is_deterministic_and_pins_come_first() {
    let c = vec![
        candidate(LabelKind::Village, 4, "Grassina", 10, 5, 8),
        candidate(LabelKind::City, 12, "Florenz", 12, 5, 7),
        candidate(LabelKind::Town, 6, "Fiesole", 40, 2, 7),
        // Under the pin's label.
        candidate(LabelKind::City, 12, "Prato", 60, 10, 5),
        candidate(LabelKind::Water, 0, "Arno", 30, 12, 4),
        candidate(LabelKind::Water, 0, "Arno", 50, 18, 4),
        candidate(LabelKind::MinorRoad, 0, "Via dei Servi", 0, 20, 13),
        candidate(LabelKind::MinorRoad, 0, "Via dei Servi", 4, 22, 13),
        candidate(LabelKind::MinorRoad, 0, "Via dei Servi", 60, 22, 13),
    ];
    let pin = Rect {
        x0: 58,
        y0: 10,
        x1: 70,
        y1: 11,
    };
    let rule = Rule {
        margin: 1,
        block: (0, 0),
        repeat: 30,
    };
    let names = |placed: &[usize], c: &[Candidate]| -> Vec<(String, i32)> {
        placed
            .iter()
            .map(|&i| (c[i].text.clone(), c[i].boxes[0].x0))
            .collect()
    };
    let placed = place(&c, &[pin], rule);
    let want: Vec<(String, i32)> = [
        ("Florenz", 12),
        ("Fiesole", 40),
        ("Arno", 30),
        ("Via dei Servi", 0),
        ("Via dei Servi", 60),
    ]
    .map(|(t, x)| (t.to_owned(), x))
    .to_vec();
    assert_eq!(names(&placed, &c), want);
    // Handed over in reverse: the same labels, the same order.
    let mut reversed = c.clone();
    reversed.reverse();
    assert_eq!(names(&place(&reversed, &[pin], rule), &reversed), want);

    // The terminal's density rule: one place per 20 x 6 block, so the
    // town in Florence's block is dropped even with room beside it.
    let dense = vec![
        candidate(LabelKind::City, 12, "Florenz", 1, 1, 7),
        candidate(LabelKind::Town, 6, "Fiesole", 12, 3, 7),
        candidate(LabelKind::Town, 6, "Prato", 21, 3, 5),
    ];
    let block = Rule {
        block: (20, 6),
        ..rule
    };
    let got = names(&place(&dense, &[], block), &dense);
    assert_eq!(got, [("Florenz".to_owned(), 1), ("Prato".to_owned(), 21)]);
    // One free cell around each: a label touching one placed does not
    // stand, one a cell apart does.
    let touching = vec![
        candidate(LabelKind::City, 12, "Aa", 0, 0, 2),
        candidate(LabelKind::City, 11, "Bb", 2, 0, 2),
        candidate(LabelKind::City, 10, "Cc", 3, 0, 2),
    ];
    let got = names(&place(&touching, &[], rule), &touching);
    assert_eq!(got, [("Aa".to_owned(), 0), ("Cc".to_owned(), 3)]);
}

/// The names a view of the Florence region shows, read through the
/// storage the way a frontend reads them: the German name where the cut
/// kept one, the local one beside it, the zoom's own classes only, and at
/// z15 the streets with their lines in the view's sub-pixels.
#[test]
fn a_view_collects_the_names_its_zoom_shows() {
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::viewport::Viewport;
    let r = Region::from_bytes(FLORENCE).expect("open");
    let at = project(LatLon::from_degrees(43.7750, 11.2610));
    let mut out = Vec::new();
    let view = Viewport::new(at, 11, 800, 600);
    // The region's box is a few pixels at z11: only what lands inside it.
    r.view_labels(&view, true, &mut out).expect("labels");
    assert!(out.iter().all(|l| l.line.is_empty()));
    out.clear();
    let view = Viewport::new(at, 13, 1200, 800);
    r.view_labels(&view, true, &mut out).expect("labels");
    assert!(out
        .iter()
        .all(|l| l.kind.is_place() || !l.line.is_empty() || l.kind == LabelKind::Park));
    out.clear();
    let view = Viewport::new(at, 15, 1024, 768);
    r.view_labels(&view, true, &mut out).expect("labels");
    let streets: Vec<&ViewLabel> = out.iter().filter(|l| l.kind.is_road()).collect();
    assert!(streets.len() > 20, "{} streets", streets.len());
    let (w, h) = (1024 * 16, 768 * 16);
    for s in &streets {
        assert!(s
            .line
            .iter()
            .flatten()
            .any(|&(x, y)| (0..w).contains(&x) && (0..h).contains(&y)));
        assert_eq!(s.text, s.local, "the streets have no German names");
    }
    // Read twice, the same names in the same order.
    let mut again = Vec::new();
    r.view_labels(&view, true, &mut again).expect("labels");
    assert_eq!(out, again);
    // Without lines no geometry is read and no street comes back.
    let mut points = Vec::new();
    r.view_labels(&view, false, &mut points).expect("labels");
    assert!(points.iter().all(|l| !l.kind.is_road()));
}

/// The pieces of one name joined end to end (1126): one ending where the
/// next starts, one running the other way, one before the first; a piece
/// apart stays apart, a piece of one point is dropped, and each line keeps
/// the least key of its pieces. The same pieces give the same lines.
#[test]
fn pieces_of_one_name_join_end_to_end() {
    let pieces = vec![
        ((5, 0), vec![(0, 0), (10, 0)]),
        ((7, 0), vec![(10, 0), (20, 0)]),
        // Runs the other way.
        ((9, 0), vec![(30, 5), (20, 0)]),
        // Ends where the first starts.
        ((3, 0), vec![(-10, 0), (0, 0)]),
        // Apart.
        ((1, 0), vec![(100, 100), (110, 100)]),
        ((0, 0), vec![(50, 50)]),
    ];
    let joined = join_pieces(pieces.clone());
    assert_eq!(
        joined,
        [
            ((3, 0), vec![(-10, 0), (0, 0), (10, 0), (20, 0), (30, 5)]),
            ((1, 0), vec![(100, 100), (110, 100)]),
        ]
    );
    assert_eq!(join_pieces(pieces), joined);
    // A ring closes and takes no more.
    let ring = vec![
        ((0, 0), vec![(0, 0), (10, 0), (10, 10)]),
        ((1, 0), vec![(10, 10), (0, 0)]),
        ((2, 0), vec![(0, 0), (-10, 0)]),
    ];
    assert_eq!(join_pieces(ring).len(), 2);
}

/// The Florence fixture at z15: its street names come as fewer, longer
/// lines than the pieces the tiles hold, and no two lines of one name
/// touch end to end (1126).
#[test]
fn a_view_joins_the_pieces_of_a_street() {
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::viewport::Viewport;
    let r = Region::from_bytes(FLORENCE).expect("open");
    let view = Viewport::new(
        project(LatLon::from_degrees(43.7750, 11.2610)),
        15,
        1024,
        768,
    );
    let mut out = Vec::new();
    r.view_labels(&view, true, &mut out).expect("labels");
    let streets: Vec<&ViewLabel> = out.iter().filter(|l| l.kind.is_road()).collect();
    let shown: std::collections::BTreeSet<String> =
        streets.iter().map(|l| l.local.clone()).collect();
    // The pieces of those names the two z15 tiles hold, by the label
    // sections alone.
    let mut pieces = 0;
    for x in [17408, 17409] {
        let id = TileId { z: 15, x, y: 11943 };
        let s = r.labels(id).expect("labels").expect("a section");
        let fx = r.fixture(id).expect("tile").expect("geometry");
        let features: Vec<_> = fx.features().collect();
        for (_, l) in s.lines() {
            if shown.contains(r.string(l.string).expect("name")) {
                pieces += features[usize::from(l.feature)].rings().count();
            }
        }
    }
    assert_eq!(
        (shown.len(), pieces, streets.len()),
        (NAMES, PIECES, JOINED),
        "names, pieces, lines"
    );
    let ends = |l: &ViewLabel| {
        let p = &l.line[0];
        [p[0], p[p.len() - 1]]
    };
    for (i, a) in streets.iter().enumerate() {
        assert_eq!(a.line.len(), 1, "one line a label");
        for b in &streets[i + 1..] {
            let same = a.kind == b.kind && a.text == b.text;
            let touch = ends(a).iter().any(|e| ends(b).contains(e));
            let closed = ends(a)[0] == ends(a)[1] || ends(b)[0] == ends(b)[1];
            assert!(!same || !touch || closed, "{} left apart", a.text);
        }
    }
}

const NAMES: usize = 75;
const PIECES: usize = 113;
const JOINED: usize = 96;

/// Rivers repeat along their line as streets do, and the terminal's
/// density block holds for streets and rivers as for places (1126).
#[test]
fn rivers_repeat_and_the_density_block_holds_for_every_kind() {
    let rule = Rule {
        margin: 1,
        block: (0, 0),
        repeat: 30,
    };
    let c = vec![
        candidate(LabelKind::River, 0, "Arno", 0, 0, 4),
        candidate(LabelKind::River, 0, "Arno", 10, 3, 4),
        candidate(LabelKind::River, 0, "Arno", 40, 3, 4),
    ];
    let placed: Vec<i32> = place(&c, &[], rule)
        .iter()
        .map(|&i| c[i].boxes[0].x0)
        .collect();
    assert_eq!(placed, [0, 40]);
    let dense = vec![
        candidate(LabelKind::City, 12, "Firenze", 1, 1, 7),
        candidate(LabelKind::River, 0, "Arno", 12, 4, 4),
        candidate(LabelKind::MinorRoad, 0, "Via dei Servi", 0, 4, 13),
        candidate(LabelKind::MinorRoad, 0, "Via Ricasoli", 22, 4, 12),
    ];
    let block = Rule {
        block: (20, 6),
        ..rule
    };
    let got: Vec<&str> = place(&dense, &[], block)
        .iter()
        .map(|&i| dense[i].text.as_str())
        .collect();
    assert_eq!(got, ["Firenze", "Via Ricasoli"]);
}
