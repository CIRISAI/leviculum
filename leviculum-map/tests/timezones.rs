//! The time zone outlines, the zone lookup, the TZif reader, the POSIX TZ
//! evaluator, the board's table and Android's packed tzdata (1684).

use std::path::{Path, PathBuf};

use leviculum_map::labels::LabelKind;
use leviculum_map::mercator::World;
use leviculum_map::outline;
use leviculum_map::timezones::{
    self, board_offset, board_rule, board_time, board_tzdata_version, packed, packed_version,
    packed_zone, posix, tzif, zone_at, zone_count, zone_index, zone_name, PosixTz,
};
use leviculum_map::Viewport;

// ---------------------------------------------------------------------
// The tables
// ---------------------------------------------------------------------

/// The baked sizes, pinned: a re-bake that changes them changes the
/// provenance in `fixtures/README.md` too.
#[test]
fn data_sizes_are_pinned() {
    assert_eq!(timezones::RINGS.len(), 1_007_112);
    assert_eq!(timezones::BORDERS.len(), 503_524);
    assert_eq!(timezones::ZONES.len(), 9_515);
    assert_eq!(timezones::RULES.len(), 2_120);
    assert_eq!(zone_count(), 63);
    assert_eq!(outline::rings(timezones::RINGS).count(), 3_004);
    assert_eq!(
        outline::rings(timezones::RINGS).filter(|r| r.hole).count(),
        1_334
    );
    assert_eq!(
        outline::rings(timezones::RINGS)
            .map(|r| r.len())
            .sum::<usize>(),
        598_780
    );
    assert_eq!(outline::lines(timezones::BORDERS).count(), 2_909);
    assert_eq!(
        outline::lines(timezones::BORDERS)
            .map(|r| r.len())
            .sum::<usize>(),
        302_268
    );
    // The world view's budget the concept sets: 1 to 3 MB.
    let total = timezones::RINGS.len() + timezones::BORDERS.len() + timezones::ZONES.len();
    assert!((1_000_000..=3_000_000).contains(&total), "{total}");
}

#[test]
fn every_ring_has_a_zone_and_every_zone_a_label() {
    let rz: Vec<_> = timezones::ring_zones().collect();
    assert_eq!(rz.len(), 3_004);
    // The rings of a zone are contiguous and the zones in index order,
    // which `zone_index_at` relies on.
    assert!(rz.windows(2).all(|w| w[0].zone <= w[1].zone));
    for z in 0..zone_count() as u16 {
        let name = zone_name(z).expect("a name");
        assert_eq!(zone_index(name), Some(z));
        assert!(
            rz.iter()
                .any(|r| r.zone == z && r.label.is_some_and(|(_, zoom)| zoom <= 10)),
            "{name} has no label"
        );
        // Every zone has a clock rule on the board.
        assert!(board_rule(name).is_some(), "{name} has no rule");
    }
    // A hole never carries a label.
    for (r, ring) in rz.iter().zip(outline::rings(timezones::RINGS)) {
        if ring.hole {
            assert!(r.label.is_none());
        }
    }
    assert_eq!(zone_name(63), None);
    let names: Vec<_> = (0..63).filter_map(zone_name).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

// ---------------------------------------------------------------------
// zone_at
// ---------------------------------------------------------------------

/// The expectations are the source's own answers: shapely's
/// point-in-polygon on the unsimplified `combined-with-oceans-now.json`
/// of release 2026f gave each of these the same zone.
#[test]
fn zone_at_answers_the_source() {
    let cases = [
        ("Berlin", 52.52, 13.405, "Europe/Paris"),
        ("New York", 40.7128, -74.006, "America/New_York"),
        ("Kathmandu (+5:45)", 27.7172, 85.324, "Asia/Kathmandu"),
        (
            "Chatham Islands (+12:45)",
            -43.95,
            -176.56,
            "Pacific/Chatham",
        ),
        ("Lord Howe Island", -31.55, 159.08, "Australia/Lord_Howe"),
        ("Sao Paulo", -23.55, -46.63, "America/Sao_Paulo"),
        // Morocco keeps +00 since 2026-09-20 (tz 2026c), merged with GMT.
        ("Casablanca", 33.57, -7.59, "Africa/Abidjan"),
        // The open Pacific in the nautical zone -12, the only one the
        // merge leaves under an Etc name.
        ("open Pacific 10 N 178 W", 10.0, -178.0, "Etc/GMT+12"),
        ("open Pacific 0 N 179.99 W", 0.0, -179.99, "Etc/GMT+12"),
        // Nautical zones that keep a land zone's time take its name.
        ("open Pacific 30 S 140 W", -30.0, -140.0, "Pacific/Gambier"),
        ("open Pacific 0 N 120 W", 0.0, -120.0, "Pacific/Pitcairn"),
        ("mid-Atlantic", 30.0, -40.0, "America/Sao_Paulo"),
        // Kiribati spans the antimeridian in three zones.
        ("Tarawa, Kiribati (+12)", 1.45, 173.0, "Pacific/Fiji"),
        (
            "Kanton, Kiribati (+13)",
            -2.81,
            -171.67,
            "Pacific/Tongatapu",
        ),
        (
            "Kiritimati, Kiribati (+14)",
            1.87,
            -157.4,
            "Pacific/Kiritimati",
        ),
        ("Fiji east of 180", -16.8, -179.9, "Pacific/Fiji"),
        ("Fiji west of 180", -18.14, 178.44, "Pacific/Fiji"),
        ("0 N 179.99 E", 0.0, 179.99, "Pacific/Fiji"),
        ("Chukotka", 65.0, -175.0, "Pacific/Fiji"),
        // Past the projection's edge, read at the edge.
        ("89 N", 89.0, 0.0, "Africa/Abidjan"),
    ];
    for (what, lat, lon, want) in cases {
        assert_eq!(zone_at(lat, lon), Some(want), "{what} ({lat}, {lon})");
    }
}

/// The outline is a gapless coverage: every point of a coarse grid over
/// the whole projection lies in exactly one zone, so `zone_at` is never
/// `None`.
#[test]
fn zone_at_covers_the_world() {
    let mut lat = -85.0;
    while lat <= 85.0 {
        let mut lon = -180.0;
        while lon < 180.0 {
            assert!(zone_at(lat, lon).is_some(), "({lat}, {lon})");
            lon += 7.3;
        }
        lat += 6.1;
    }
}

// ---------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------

#[test]
fn labels_and_zones_of_a_view() {
    // The whole world at z2, 1024 pixels square: 58 polygons hold a
    // 20-pixel circle there, at z1 only 7.
    let world = Viewport::new(
        World {
            x: 1 << 31,
            y: 1 << 31,
        },
        2,
        1024,
        1024,
    );
    let mut out = Vec::new();
    timezones::labels(&world, &mut out);
    assert_eq!(out.len(), 58);
    for l in &out {
        assert_eq!(l.kind, LabelKind::TimeZone);
        assert!(zone_index(&l.text).is_some(), "{}", l.text);
        assert_eq!(l.text, l.local);
        assert!(l.min_zoom <= 2);
        assert!(l.line.is_empty());
        let (x, y) = l.anchor;
        assert!((0..1024 * 16).contains(&x) && (0..1024 * 16).contains(&y));
    }
    let mut zones = Vec::new();
    timezones::zones_in_view(&world, &mut zones);
    assert_eq!(zones.len(), 63);

    // Berlin at z8: its zone is in view, the open Pacific's is not, and
    // its label's anchor (in France or the sea) need not be.
    let berlin =
        leviculum_map::mercator::project(leviculum_map::LatLon::from_degrees(52.52, 13.405));
    let view = Viewport::new(berlin, 8, 400, 300);
    let mut zones = Vec::new();
    timezones::zones_in_view(&view, &mut zones);
    let paris = zone_index("Europe/Paris").expect("a zone");
    assert!(zones.contains(&paris));
    assert!(!zones.contains(&zone_index("Etc/GMT+12").expect("a zone")));
    // Each zone in it has a ring whose box meets the view, and the rings
    // reach a frontend through `zone_rings`.
    assert!(timezones::zone_rings().any(|(z, _)| z == paris));
    // The borders draw through the outline's line drawer.
    assert_eq!(
        outline::bits(timezones::BOUNDARIES.borders),
        outline::bits(timezones::RINGS)
    );
}

// ---------------------------------------------------------------------
// POSIX TZ strings
// ---------------------------------------------------------------------

/// glibc 2.41's answers (GNU date 9.7, `TZ=<string> date -d @T '+%::z
/// %Z'`, on 2026-11-11), each on both sides of a transition.
const POSIX_REFERENCE: &[(&str, i64, i32, &str)] = &[
    ("CET-1CEST,M3.5.0,M10.5.0/3", 1774745999, 3600, "CET"), // 2026-03-29T00:59:59Z
    ("CET-1CEST,M3.5.0,M10.5.0/3", 1774746000, 7200, "CEST"), // 2026-03-29T01:00:00Z
    ("CET-1CEST,M3.5.0,M10.5.0/3", 1792889999, 7200, "CEST"), // 2026-10-25T00:59:59Z
    ("CET-1CEST,M3.5.0,M10.5.0/3", 1792890000, 3600, "CET"), // 2026-10-25T01:00:00Z
    ("IST-2IDT,M3.4.4/26,M10.5.0", 1774569599, 7200, "IST"), // 2026-03-26T23:59:59Z
    ("IST-2IDT,M3.4.4/26,M10.5.0", 1774569600, 10800, "IDT"), // 2026-03-27T00:00:00Z
    ("IST-2IDT,M3.4.4/26,M10.5.0", 1792882799, 10800, "IDT"), // 2026-10-24T22:59:59Z
    ("IST-2IDT,M3.4.4/26,M10.5.0", 1792882800, 7200, "IST"), // 2026-10-24T23:00:00Z
    ("EST5EDT,M3.2.0,M11.1.0", 1772953199, -18000, "EST"),   // 2026-03-08T06:59:59Z
    ("EST5EDT,M3.2.0,M11.1.0", 1772953200, -14400, "EDT"),   // 2026-03-08T07:00:00Z
    ("EST5EDT,M3.2.0,M11.1.0", 1793512799, -14400, "EDT"),   // 2026-11-01T05:59:59Z
    ("EST5EDT,M3.2.0,M11.1.0", 1793512800, -18000, "EST"),   // 2026-11-01T06:00:00Z
    // The southern hemisphere: DST spans the new year.
    ("AEST-10AEDT,M10.1.0,M4.1.0/3", 1775318399, 39600, "AEDT"), // 2026-04-04T15:59:59Z
    ("AEST-10AEDT,M10.1.0,M4.1.0/3", 1775318400, 36000, "AEST"), // 2026-04-04T16:00:00Z
    ("AEST-10AEDT,M10.1.0,M4.1.0/3", 1791043199, 36000, "AEST"), // 2026-10-03T15:59:59Z
    ("AEST-10AEDT,M10.1.0,M4.1.0/3", 1791043200, 39600, "AEDT"), // 2026-10-03T16:00:00Z
    ("AEST-10AEDT,M10.1.0,M4.1.0/3", 1798758000, 39600, "AEDT"), // 2026-12-31T23:00:00Z
    ("AEST-10AEDT,M10.1.0,M4.1.0/3", 1798763400, 39600, "AEDT"), // 2027-01-01T00:30:00Z
    ("AEST-10AEDT,M10.1.0,M4.1.0/3", 1782907200, 36000, "AEST"), // 2026-07-01T12:00:00Z
    ("NZST-12NZDT,M9.5.0,M4.1.0/3", 1767225600, 46800, "NZDT"),  // 2026-01-01T00:00:00Z
    ("NZST-12NZDT,M9.5.0,M4.1.0/3", 1782864000, 43200, "NZST"),  // 2026-07-01T00:00:00Z
    ("NZST-12NZDT,M9.5.0,M4.1.0/3", 1790431199, 43200, "NZST"),  // 2026-09-26T13:59:59Z
    ("NZST-12NZDT,M9.5.0,M4.1.0/3", 1790431200, 46800, "NZDT"),  // 2026-09-26T14:00:00Z
    // RFC 8536's extensions: a negative time (Nuuk) and one past 24
    // hours (Gaza).
    ("<-02>2<-01>,M3.5.0/-1,M10.5.0/0", 1774745999, -7200, "-02"), // 2026-03-29T00:59:59Z
    ("<-02>2<-01>,M3.5.0/-1,M10.5.0/0", 1774746000, -3600, "-01"), // 2026-03-29T01:00:00Z
    ("<-02>2<-01>,M3.5.0/-1,M10.5.0/0", 1792889999, -3600, "-01"), // 2026-10-25T00:59:59Z
    ("<-02>2<-01>,M3.5.0/-1,M10.5.0/0", 1792890000, -7200, "-02"), // 2026-10-25T01:00:00Z
    ("EET-2EEST,M3.4.4/50,M10.4.4/50", 1774655999, 7200, "EET"),   // 2026-03-27T23:59:59Z
    ("EET-2EEST,M3.4.4/50,M10.4.4/50", 1774656000, 10800, "EEST"), // 2026-03-28T00:00:00Z
    ("EET-2EEST,M3.4.4/50,M10.4.4/50", 1792796399, 10800, "EEST"), // 2026-10-23T22:59:59Z
    ("EET-2EEST,M3.4.4/50,M10.4.4/50", 1792796400, 7200, "EET"),   // 2026-10-23T23:00:00Z
    // `Jn` never counts February 29, `n` does; 2028 is a leap year.
    ("XXX3YYY,J60/0,J300/0", 1835492399, -10800, "XXX"), // 2028-03-01T02:59:59Z
    ("XXX3YYY,J60/0,J300/0", 1835492400, -7200, "YYY"),  // 2028-03-01T03:00:00Z
    ("XXX3YYY,J60/0,J300/0", 1856224799, -7200, "YYY"),  // 2028-10-27T01:59:59Z
    ("XXX3YYY,J60/0,J300/0", 1856224800, -10800, "XXX"), // 2028-10-27T02:00:00Z
    ("XXX3YYY,59/0,299/0", 1835405999, -10800, "XXX"),   // 2028-02-29T02:59:59Z
    ("XXX3YYY,59/0,299/0", 1835406000, -7200, "YYY"),    // 2028-02-29T03:00:00Z
    ("XXX3YYY,59/0,299/0", 1856138399, -7200, "YYY"),    // 2028-10-26T01:59:59Z
    ("XXX3YYY,59/0,299/0", 1856138400, -10800, "XXX"),   // 2028-10-26T02:00:00Z
    ("EST5EDT,0/0,J365/25", 1767243600, -14400, "EDT"),  // 2026-01-01T05:00:00Z
    ("EST5EDT,0/0,J365/25", 1782907200, -14400, "EDT"),  // 2026-07-01T12:00:00Z
    ("EST5EDT,0/0,J365/25", 1798761599, -14400, "EDT"),  // 2026-12-31T23:59:59Z
    ("EST5EDT,0/0,J365/25", 1798779600, -14400, "EDT"),  // 2027-01-01T05:00:00Z
    ("<+0545>-5:45", 1782907200, 20700, "+0545"),        // 2026-07-01T12:00:00Z
];

#[test]
fn posix_matches_glibc() {
    for &(tz, t, off, abbr) in POSIX_REFERENCE {
        let got = posix::offset_at(tz, t).unwrap_or_else(|| panic!("{tz} parses"));
        assert_eq!((got.utc_offset, got.abbr), (off, abbr), "{tz} at {t}");
        let dst = ["CEST", "IDT", "EDT", "AEDT", "NZDT", "-01", "EEST", "YYY"].contains(&abbr);
        assert_eq!(got.dst, dst, "{tz} at {t}");
    }
}

/// RFC 8536 section 3.3.1 gives `EST5EDT,0/0,J365/25` as DST all year,
/// and so does this evaluator. glibc 2.41 answers EST for the last five
/// hours of each UTC year (1767225600 and 1767243599, 2026-01-01T00:00Z
/// and 04:59:59Z): it evaluates the rule in the UTC year, whose start
/// lies five hours ahead. The RFC wins.
#[test]
fn posix_dst_all_year_is_dst_at_the_new_year() {
    for t in [1767225600, 1767243599] {
        let got = posix::offset_at("EST5EDT,0/0,J365/25", t).expect("parses");
        assert_eq!((got.utc_offset, got.dst), (-14400, true), "{t}");
    }
}

#[test]
fn posix_parses_its_parts() {
    let tz = PosixTz::parse("<+1245>-12:45<+1345>,M9.5.0/2:45,M4.1.0/3:45").expect("parses");
    assert_eq!(tz.abbr, "+1245");
    assert_eq!(tz.utc_offset, 45_900);
    let dst = tz.dst.expect("dst");
    assert_eq!(dst.abbr, "+1345");
    assert_eq!(dst.utc_offset, 49_500);
    assert_eq!(dst.start.time, 9_900);
    assert_eq!(
        dst.start.date,
        posix::Date::Month {
            month: 9,
            week: 5,
            day: 0
        }
    );
    // Without a DST offset it is one hour east of standard; without a
    // rule it is the US rule.
    let tz = PosixTz::parse("EST5EDT").expect("parses");
    let dst = tz.dst.expect("dst");
    assert_eq!(dst.utc_offset, -14_400);
    assert_eq!(dst.start.time, 7200);
    assert_eq!(
        dst.end.date,
        posix::Date::Month {
            month: 11,
            week: 1,
            day: 0
        }
    );
    assert_eq!(
        PosixTz::parse("GMT0").map(|t| (t.utc_offset, t.dst)),
        Some((0, None))
    );
    assert_eq!(
        PosixTz::parse("<-03>3").map(|t| t.utc_offset),
        Some(-10_800)
    );
    assert_eq!(
        PosixTz::parse("NST3:30NDT,M3.2.0,M11.1.0").map(|t| t.utc_offset),
        Some(-12_600)
    );
    for bad in [
        "",
        "C1",
        "CET",
        "CET-1CEST,M3.5.0",
        "CET-1CEST,M13.1.0,M10.5.0",
        "CET-1CEST,M3.0.0,M10.5.0",
        "CET-1CEST,M3.5.7,M10.5.0",
        "CET-1CEST,J0,J300",
        "CET-1CEST,366,300",
        "<+05",
        "<+0>-5",
        "CET-1 ",
        "CET-1CEST,M3.5.0,M10.5.0/3x",
        "CET-1:60",
    ] {
        assert_eq!(PosixTz::parse(bad), None, "{bad:?}");
    }
}

// ---------------------------------------------------------------------
// TZif against the host's tzdata
// ---------------------------------------------------------------------

fn host_zone(name: &str) -> Vec<u8> {
    let path = Path::new("/usr/share/zoneinfo").join(name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; the host needs tzdata (apt install tzdata)",
            path.display()
        )
    })
}

/// The host's libc on the same files: GNU date 9.7, glibc 2.41, Debian's
/// tzdata 2026b-0+deb13u1 (`TZ=<zone> date -d @T '+%::z %Z'`, run
/// 2026-11-11). The zones and instants are chosen so that no tzdata
/// release since has changed the answer.
const TZIF_REFERENCE: &[(&str, i64, i32, bool, &str)] = &[
    ("Europe/Berlin", 1782907200, 7200, true, "CEST"), // 2026-07-01T12:00Z, summer
    ("Europe/Berlin", 1768478400, 3600, false, "CET"), // 2026-01-15T12:00Z, winter
    ("Europe/Berlin", -2840140800, 3208, false, "LMT"), // 1880-01-01, before the first transition
    ("Europe/Berlin", 4118083200, 7200, true, "CEST"), // 2100-07-01, past the last: the footer
    ("America/Sao_Paulo", 1768478400, -10800, false, "-03"), // 2026-01-15, no DST since 2019
    ("America/Sao_Paulo", 1516017600, -7200, true, "-02"), // 2018-01-15, still DST then
    ("Australia/Lord_Howe", 1768478400, 39600, true, "+11"), // 2026-01-15, half-hour DST
    ("Australia/Lord_Howe", 1782907200, 37800, false, "+1030"), // 2026-07-01
    ("Asia/Kathmandu", 1782907200, 20700, false, "+0545"),
    ("Pacific/Chatham", 1768478400, 49500, true, "+1345"),
    ("Pacific/Chatham", 1782907200, 45900, false, "+1245"),
    ("America/New_York", 1782907200, -14400, true, "EDT"),
    ("Pacific/Kiritimati", 1782907200, 50400, false, "+14"),
    // Ramadan 2026, an explicit transition no footer can express.
    ("Africa/Casablanca", 1772366400, 0, true, "+00"), // 2026-03-01T12:00Z
];

#[test]
fn tzif_matches_the_hosts_libc() {
    for &(zone, t, off, dst, abbr) in TZIF_REFERENCE {
        let bytes = host_zone(zone);
        let got = tzif::offset_at(&bytes, t).unwrap_or_else(|| panic!("{zone} reads"));
        assert_eq!(got.utc_offset, off, "{zone} at {t}");
        assert_eq!(got.abbr, abbr, "{zone} at {t}");
        // Casablanca's +00 in Ramadan is flagged DST in tzdata (its
        // "negative DST"); the others as the abbreviation says.
        if zone != "Africa/Casablanca" {
            assert_eq!(got.dst, dst, "{zone} at {t}");
        }
    }
    // The footer of a version 2+ file.
    assert_eq!(
        tzif::footer(&host_zone("Europe/Berlin")),
        Some("CET-1CEST,M3.5.0,M10.5.0/3")
    );
}

/// A version 1 file, as an old system writes it, and a file without
/// transitions, built here: header, 32-bit data, no footer.
#[test]
fn tzif_reads_version_1_and_no_transitions() {
    fn v1(times: &[(i32, u8)], types: &[(i32, u8, u8)], chars: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"TZif\0");
        b.extend_from_slice(&[0; 15]);
        for n in [0, 0, 0, times.len(), types.len(), chars.len()] {
            b.extend_from_slice(&(n as u32).to_be_bytes());
        }
        for (t, _) in times {
            b.extend_from_slice(&t.to_be_bytes());
        }
        b.extend(times.iter().map(|&(_, i)| i));
        for &(off, dst, idx) in types {
            b.extend_from_slice(&off.to_be_bytes());
            b.extend_from_slice(&[dst, idx]);
        }
        b.extend_from_slice(chars);
        b
    }
    let file = v1(
        &[(1000, 1), (2000, 0)],
        &[(3600, 0, 0), (7200, 1, 4)],
        b"STD\0DST\0",
    );
    let at = |t| tzif::offset_at(&file, t).map(|o| (o.utc_offset, o.dst, o.abbr));
    assert_eq!(at(0), Some((3600, false, "STD")));
    assert_eq!(at(1000), Some((7200, true, "DST")));
    assert_eq!(at(1999), Some((7200, true, "DST")));
    assert_eq!(at(5000), Some((3600, false, "STD")));
    assert_eq!(tzif::footer(&file), None);
    let none = v1(&[], &[(-43200, 0, 0)], b"-12\0");
    assert_eq!(
        tzif::offset_at(&none, 0).map(|o| o.utc_offset),
        Some(-43200)
    );
    // Cut short, a bad index, a bad magic: never a panic.
    for n in 0..file.len() {
        assert_eq!(tzif::offset_at(&file[..n], 1500), None, "cut at {n}");
    }
    let mut bad = file.clone();
    bad[44 + 8] = 7;
    assert_eq!(tzif::offset_at(&bad, 1500), None);
    let mut bad = file.clone();
    bad[0] = b'X';
    assert_eq!(tzif::offset_at(&bad, 1500), None);
}

// ---------------------------------------------------------------------
// The board's table
// ---------------------------------------------------------------------

fn fixture_zone(name: &str) -> Vec<u8> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "fixtures/zoneinfo-2026f", name]
        .iter()
        .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The board's POSIX strings against the TZif files of the tzdata they
/// were generated from (`fixtures/zoneinfo-2026f`, zic 2026f `-b fat`, so
/// the transitions through 2037 are explicit and the footer is checked
/// against them, not against itself), every hour of 2026, every zone.
/// The one disagreement is the known limit: Cuba stays on -04 from
/// 2026-11-01 through 2027-11-07 (tz 2026f), an explicit exception the
/// footer, Havana's lasting rule, cannot say.
#[test]
fn board_agrees_with_tzif_through_2026() {
    assert_eq!(board_tzdata_version(), Some("2026f"));
    let start = 1767225600i64; // 2026-01-01T00:00Z
    let end = 1798761600i64; // 2027-01-01T00:00Z
    let mut wrong: Vec<(&str, i64)> = Vec::new();
    for z in 0..zone_count() as u16 {
        let name = zone_name(z).expect("a name");
        let bytes = fixture_zone(name);
        let mut t = start;
        while t < end {
            let file = tzif::offset_at(&bytes, t).expect("reads");
            let board = board_time(name, t).expect("a rule");
            if (file.utc_offset, file.dst) != (board.utc_offset, board.dst) {
                wrong.push((name, t));
            } else {
                assert_eq!(file.abbr, board.abbr, "{name} at {t}");
            }
            assert_eq!(board_offset(name, t), Some(board.utc_offset));
            t += 3600;
        }
    }
    // Havana from the hour of the fall-back that did not happen,
    // 2026-11-01T05:00Z (01:00 CDT), to the end of the year.
    let havana_from = 1793509200i64;
    let expected: Vec<(&str, i64)> = (havana_from..end)
        .step_by(3600)
        .map(|t| ("America/Havana", t))
        .collect();
    assert_eq!(wrong, expected);
    assert_eq!(
        board_time("America/Havana", 1794571200).map(|o| o.utc_offset),
        Some(-18000)
    );
    assert_eq!(
        tzif::offset_at(&fixture_zone("America/Havana"), 1794571200).map(|o| o.utc_offset),
        Some(-14400)
    );
}

#[test]
fn board_rules() {
    assert_eq!(
        board_rule("Europe/Paris"),
        Some("CET-1CEST,M3.5.0,M10.5.0/3")
    );
    assert_eq!(board_rule("Asia/Kathmandu"), Some("<+0545>-5:45"));
    assert_eq!(board_rule("Europe/Berlin"), None);
    assert_eq!(board_offset("Asia/Kathmandu", 0), Some(20_700));
    assert_eq!(board_offset("Pacific/Chatham", 1768478400), Some(49_500));
    assert_eq!(board_offset("Nowhere/Nothing", 0), None);
}

// ---------------------------------------------------------------------
// Android's packed tzdata
// ---------------------------------------------------------------------

/// A packed file as bionic's `bionic_tzdata_header_t` and `index_entry_t`
/// lay it out: header, index, data.
fn packed_file(zones: &[(&str, &[u8])]) -> Vec<u8> {
    let index = packed::PACKED_HEADER;
    let data = index + zones.len() * packed::PACKED_ENTRY;
    let mut b = Vec::new();
    b.extend_from_slice(b"tzdata2026f\0");
    b.extend_from_slice(&(index as i32).to_be_bytes());
    b.extend_from_slice(&(data as i32).to_be_bytes());
    let total: usize = zones.iter().map(|(_, z)| z.len()).sum();
    b.extend_from_slice(&((data + total) as i32).to_be_bytes());
    let mut at = 0usize;
    for (name, z) in zones {
        let mut n = [0u8; 40];
        n[..name.len()].copy_from_slice(name.as_bytes());
        b.extend_from_slice(&n);
        b.extend_from_slice(&(at as i32).to_be_bytes());
        b.extend_from_slice(&(z.len() as i32).to_be_bytes());
        b.extend_from_slice(&0i32.to_be_bytes());
        at += z.len();
    }
    for (_, z) in zones {
        b.extend_from_slice(z);
    }
    b
}

#[test]
fn android_packed_tzdata() {
    let paris = fixture_zone("Europe/Paris");
    let kathmandu = fixture_zone("Asia/Kathmandu");
    // A name of exactly 40 bytes has no NUL in its entry.
    let long = "America/Argentina/ComodRivadavia/Exactly";
    assert_eq!(long.len(), 40);
    let file = packed_file(&[
        ("Asia/Kathmandu", &kathmandu),
        ("Europe/Paris", &paris),
        (long, b"long-zone"),
    ]);
    assert_eq!(packed_version(&file), Some("2026f"));
    assert_eq!(packed_zone(&file, "Europe/Paris"), Some(&paris[..]));
    assert_eq!(packed_zone(&file, "Asia/Kathmandu"), Some(&kathmandu[..]));
    assert_eq!(packed_zone(&file, long), Some(&b"long-zone"[..]));
    assert_eq!(packed_zone(&file, "Europe/Pari"), None);
    assert_eq!(packed_zone(&file, "Europe/Berlin"), None);
    let off = packed_zone(&file, "Asia/Kathmandu")
        .and_then(|z| tzif::offset_at(z, 1782907200))
        .map(|o| o.utc_offset);
    assert_eq!(off, Some(20_700));
    // A bad magic, an index that is not whole entries, a cut file.
    let mut bad = file.clone();
    bad[0] = b'x';
    assert_eq!(packed_zone(&bad, "Europe/Paris"), None);
    let mut bad = file.clone();
    bad[19] += 1;
    assert_eq!(packed_zone(&bad, "Europe/Paris"), None);
    for n in 0..file.len() {
        let _ = packed_zone(&file[..n], "Europe/Paris");
    }
    assert_eq!(packed_zone(&file[..file.len() - 1], long), None);

    #[cfg(feature = "std")]
    {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("timezones-packed-tzdata");
        std::fs::write(&path, &file).expect("writes");
        assert_eq!(
            timezones::system::packed_zone_bytes(&path, "Europe/Paris"),
            Some(paris.clone())
        );
        // Not TZif bytes.
        assert_eq!(timezones::system::packed_zone_bytes(&path, long), None);
        assert_eq!(
            timezones::system::packed_zone_bytes(&path, "../Paris"),
            None
        );
    }
}

// ---------------------------------------------------------------------
// The system's tzdata (feature std)
// ---------------------------------------------------------------------

#[cfg(feature = "std")]
#[test]
fn system_zone_bytes_reads_zoneinfo() {
    use timezones::system_zone_bytes;
    let berlin = system_zone_bytes("Europe/Berlin").expect("the host has tzdata");
    assert_eq!(berlin, host_zone("Europe/Berlin"));
    assert_eq!(
        tzif::offset_at(&berlin, 1782907200).map(|o| o.utc_offset),
        Some(7200)
    );
    for bad in [
        "",
        "/etc/passwd",
        "../../etc/passwd",
        "Europe/../../etc/passwd",
        "Europe\\Berlin",
        "Nowhere/Nothing",
        // Not a TZif file.
        "zone.tab",
    ] {
        assert_eq!(system_zone_bytes(bad), None, "{bad:?}");
    }
}
