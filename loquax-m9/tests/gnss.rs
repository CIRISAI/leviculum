//! The M9's own position (1477): the AG3352's sentences into the fix the
//! model takes, the sky the map's corner says, and when the receiver is
//! powered.
//!
//! The searching sentences are the board's own: window 22 (2026-11-09,
//! `/home/lew/rig-run/m9/2026-11-09d/map/board.log`, the `[GNSS] nmea=`
//! lines) recorded the first twelve lines of a power-up indoors, no sky.
//! Seven are `$PAIR` sentences, all of them before the init's first
//! command went out (`pair-sleep-lock sent` at t=6406, the last of them
//! at t=6355): the module's own power-up messages, not answers to us.
//! The sentences of a fix are still WRITTEN, in the shape the recorded
//! ones have (`GN` for the combined fix, a talker per constellation for
//! GSV, the signal id last), with their checksums computed here, until a
//! window with a sky records them.

use leviculum_lxmf::telemetry::Location;
use loquax::ui::strings::Lang;
use loquax::ui::{update, UiEvent, UiModel};
use loquax_m9::field::Field;
use loquax_m9::fixture::Screen;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::gnss::{self, Duty, Lines, Nmea, Receiver, Sky};
use loquax_m9::map::{self, Map, FIXTURE};
use loquax_m9::render::Renderer;
use loquax_m9::screen::Look;

mod support;

/// `body` as a sentence on the line: `$`, the body, its checksum, CRLF.
fn line(body: &str) -> Vec<u8> {
    let sum = body.bytes().fold(0u8, |a, b| a ^ b);
    format!("${body}*{sum:02X}\r\n").into_bytes()
}

/// The first twelve lines of a power-up on the board, verbatim (window
/// 22): the module's `$PAIR` power-up messages, then a second without a
/// sky, no satellite in view in any of the four constellations.
const RECORDED: [&str; 12] = [
    "$PAIR001,002,1*38",
    "$PAIR001,021,0*38",
    "$PAIR011,001*27",
    "$PAIR010,1,-1*16",
    "$PAIR010,2,-1*15",
    "$PAIR001,002,0*39",
    "$PAIR001,021,0*38",
    "$GNGGA,235942.013,,,,,0,0,,,M,,M,,*5F",
    "$GPGSV,1,1,00,1*64",
    "$GLGSV,1,1,00,1*78",
    "$GAGSV,1,1,00,7*73",
    "$GBGSV,1,1,00,1*76",
];

/// The recorded lines as the UART brings them, CRLF after each.
fn searching() -> Vec<u8> {
    RECORDED
        .iter()
        .flat_map(|l| [l.as_bytes(), b"\r\n"].concat())
        .collect()
}

/// WRITTEN, no sky recorded yet: a searching second with satellites in
/// view over two constellations, for the sum the corner shows.
fn searching_in_view() -> Vec<u8> {
    [
        line("GNGGA,053010.000,,,,,0,0,,,M,,M,,"),
        line("GPGSV,1,1,03,05,40,083,,13,22,301,,15,61,180,,1"),
        line("GLGSV,1,1,01,70,35,120,,1"),
        line("GNRMC,053010.000,V,,,,,,,091126,,,N,V"),
    ]
    .concat()
}

/// WRITTEN, no sky recorded yet: a second with a fix over Florence, 8 in
/// use, HDOP 0.9, 62.3 m.
fn fixed(time: &str) -> Vec<u8> {
    [
        line(&format!(
            "GNGGA,{time},4346.4000,N,01115.3600,E,1,08,0.9,62.3,M,47.0,M,,"
        )),
        line("GPGSV,2,1,06,05,40,083,32,13,22,301,28,15,61,180,40,18,10,040,20,1"),
        line(&format!(
            "GNRMC,{time},A,4346.4000,N,01115.3600,E,1.94,87.50,091126,,,A,V"
        )),
    ]
    .concat()
}

/// Feed `bytes` at `now_ms`; the fixes made.
fn feed(rx: &mut Receiver, bytes: &[u8], now_ms: u64) -> Vec<Location> {
    let mut lines = Lines::new();
    let mut out = Vec::new();
    lines.push(bytes, |l| out.extend(rx.line(l, now_ms)));
    out
}

#[test]
fn the_three_sentences_read_with_their_fields() {
    let gga = line("GNGGA,053012.000,4346.4000,N,01115.3600,E,1,08,0.9,62.3,M,47.0,M,,");
    assert_eq!(
        gnss::parse(&gga[..gga.len() - 2]),
        Some(Nmea::Gga {
            quality: 1,
            sats: 8,
            hdop_e1: Some(9),
            // 62.3 m over sea level and the geoid 47.0 m over the
            // ellipsoid: 109.3 m over the ellipsoid, the wire's height (1684).
            alt_e2: Some(10930),
            at: Some((43_773_333, 11_256_000)),
        })
    );
    let rmc = line("GNRMC,053012.000,A,4346.4000,N,01115.3600,W,1.94,87.50,091126,,,A,V");
    assert_eq!(
        gnss::parse(&rmc[..rmc.len() - 2]),
        Some(Nmea::Rmc {
            valid: true,
            at: Some((43_773_333, -11_256_000)),
            // 1.94 kn is 0.998 m/s.
            speed_e2: Some(99),
            course_e2: Some(8750),
            unix: Some(1_794_202_212),
        })
    );
    let gsv = line("GBGSV,1,1,02,07,40,083,,10,22,301,,1");
    assert_eq!(
        gnss::parse(&gsv[..gsv.len() - 2]),
        Some(Nmea::Gsv {
            talker: *b"GB",
            in_view: 2
        })
    );
    // The AG3352's own sentences read as what they are: an answer to a
    // command, another `$PAIR`, a standard sentence the board does not
    // read. None of them is a fix's sentence, none is rejected.
    assert_eq!(
        gnss::parse(b"$PAIR001,002,1*38"),
        Some(Nmea::Ack {
            command: 2,
            result: 1
        })
    );
    assert_eq!(
        gnss::parse(b"$PAIR010,1,-1*16"),
        Some(Nmea::Pair { command: 10 })
    );
    let version = line("PAIR021,AG3352,V1.0");
    assert_eq!(
        gnss::parse(&version[..version.len() - 2]),
        Some(Nmea::Pair { command: 21 })
    );
    let gll = line("GNGLL,,,,,235942.013,V,N");
    assert_eq!(
        gnss::parse(&gll[..gll.len() - 2]),
        Some(Nmea::Unread {
            talker: *b"GN",
            kind: *b"GLL"
        })
    );
    // A head that is no sentence is none.
    let junk = line("PAIRX,1");
    assert_eq!(gnss::parse(&junk[..junk.len() - 2]), None);
}

/// Every recorded line passes the parser's checksum as recorded, the
/// `$GAGSV` with its signal id 7 included: each reads, none is `None`.
#[test]
fn every_recorded_line_passes_its_checksum() {
    for l in RECORDED {
        assert!(gnss::parse(l.as_bytes()).is_some(), "{l}");
    }
    assert_eq!(
        gnss::parse(b"$GAGSV,1,1,00,7*73"),
        Some(Nmea::Gsv {
            talker: *b"GA",
            in_view: 0
        })
    );
    // The control: the same line with the signal id the instruction's copy
    // had (1) fails the recorded checksum.
    assert_eq!(gnss::parse(b"$GAGSV,1,1,00,1*73"), None);
}

/// The recorded power-up as the receiver counts it: five sentences read
/// (the GGA, four GSV), the seven `$PAIR` lines replies, nothing
/// rejected; searching with none in view. Then the lines the window's
/// `rejected=14` most likely were: GLL, GSA, VTG or ZDA, standard
/// sentences the init switches off, which are unread, not rejected; only
/// a bad checksum is.
#[test]
fn the_recorded_power_up_counts_its_lines_by_what_they_are() {
    let mut rx = Receiver::new();
    rx.power(true, 5_774);
    assert!(feed(&mut rx, &searching(), 6_405).is_empty());
    assert_eq!(
        (rx.sentences, rx.replies, rx.unread, rx.rejected),
        (5, 7, 0, 0)
    );
    assert_eq!(rx.sky(6_405), Sky::Searching { in_view: 0 });
    assert!(rx.line_words(6_405).starts_with(
        "state=searching sats=0 in_view=0 hdop=none age_s=none \
         sentences=5 replies=7 unread=0 rejected=0"
    ));

    let unread = [
        line("GNGLL,,,,,235943.013,V,N"),
        line("GNGSA,A,1,,,,,,,,,,,,,,,,1"),
        line("GNVTG,,T,,M,,N,,K,N"),
        line("GNZDA,235943.013,,,,,"),
    ]
    .concat();
    feed(&mut rx, &unread, 7_000);
    assert_eq!((rx.unread, rx.rejected), (4, 0));
    let mut bad = line("GNGLL,,,,,235944.013,V,N");
    bad[3] ^= 0x01;
    feed(&mut rx, &bad, 8_000);
    assert_eq!((rx.unread, rx.rejected), (4, 1));
    assert_eq!(
        rx.sentences, 5,
        "the init's gate counts read sentences only"
    );
}

/// Off, then searching with the satellites in view summed over the
/// constellations, then a fix the model can take: Florence, at the RMC's
/// time, HDOP 0.9 as 4.5 m. A fix ages; a lost sky searches again and
/// keeps the last fix. A line cut in two by the UART reads whole; a
/// corrupt one is counted, not read.
#[test]
fn the_receiver_goes_from_off_to_searching_to_a_fix() {
    let mut rx = Receiver::new();
    assert_eq!(rx.sky(0), Sky::Off);
    rx.power(true, 1_000);
    assert!(feed(&mut rx, &searching_in_view(), 2_000).is_empty());
    assert_eq!(rx.sky(2_000), Sky::Searching { in_view: 4 });
    assert!(rx
        .line_words(2_000)
        .starts_with("state=searching sats=0 in_view=4 hdop=none age_s=none"));

    let bytes = fixed("053012.000");
    let (a, b) = bytes.split_at(37);
    let mut lines = Lines::new();
    let mut fixes = Vec::new();
    lines.push(a, |l| fixes.extend(rx.line(l, 3_000)));
    assert!(fixes.is_empty(), "half a line is no line");
    lines.push(b, |l| fixes.extend(rx.line(l, 3_000)));
    assert_eq!(fixes.len(), 1);
    let fix = fixes[0];
    assert_eq!(
        (fix.latitude_e6, fix.longitude_e6),
        (43_773_333, 11_256_000)
    );
    assert_eq!(fix.last_update, 1_794_202_212);
    assert_eq!(fix.accuracy_e2, 450);
    // Over the ellipsoid: 62.3 m over sea level plus the 47.0 m geoid.
    assert_eq!(fix.altitude_e2, 10930);
    assert_eq!(rx.sky(3_000), Sky::Fix { age_s: 0 });
    assert_eq!(rx.sky(15_500), Sky::Fix { age_s: 12 });
    assert!(rx
        .line_words(15_500)
        .starts_with("state=fix sats=8 in_view=7 hdop=0.9 age_s=12"));

    // The sky lost: searching again, the last fix kept.
    feed(&mut rx, &searching_in_view(), 20_000);
    assert!(matches!(rx.sky(20_000), Sky::Searching { .. }));
    assert_eq!(rx.last().map(|(l, _)| l), Some(fix));

    // A corrupt line is counted and not read.
    let before = rx.sentences;
    let mut bad = fixed("053013.000");
    bad[20] ^= 0x01;
    feed(
        &mut rx,
        &bad[..bad.iter().position(|&b| b == b'\n').unwrap_or(0) + 1],
        21_000,
    );
    assert_eq!(rx.sentences, before);
    assert_eq!(rx.rejected, 1);

    // Cut: off, its sky gone, the last fix still there.
    rx.power(false, 22_000);
    assert_eq!(rx.sky(22_000), Sky::Off);
    assert!(rx.last().is_some());
}

/// The words of the map's corner, both languages.
#[test]
fn the_corner_says_searching_and_the_fixs_age() {
    assert_eq!(gnss::sky_words(Sky::Off, Lang::De), None);
    assert_eq!(
        gnss::sky_words(Sky::Searching { in_view: 4 }, Lang::De).as_deref(),
        Some("Suche Satelliten… 4")
    );
    assert_eq!(
        gnss::sky_words(Sky::Fix { age_s: 12 }, Lang::De).as_deref(),
        Some("Fix vor 12 s")
    );
    assert_eq!(
        gnss::sky_words(Sky::Fix { age_s: 300 }, Lang::En).as_deref(),
        Some("Fix 5 min ago")
    );
    assert_eq!(gnss::age_words(7_300, Lang::De), "2 h");
}

/// The duty (1477 §1): off until something asks; on while the map is open
/// or a share runs; after that a refresh when the cadence says one is due,
/// which ends once its fix has settled ten seconds or after two minutes
/// without one. Two refresh fixes 50 m apart make the cadence five
/// minutes, else it is an hour.
#[test]
fn the_receiver_is_powered_when_asked_and_on_the_cadence() {
    let mut duty = Duty::new();
    let mut rx = Receiver::new();
    assert!(!duty.on(false, &rx, 0), "never asked: off");
    assert!(duty.on(true, &rx, 1_000), "the map is open");
    rx.power(true, 1_000);
    feed(&mut rx, &fixed("053012.000"), 5_000);
    // The map closed: the fix is fresh, the next refresh an hour on.
    assert!(!duty.on(false, &rx, 6_000));
    assert!(!duty.on(false, &rx, 5_000 + gnss::STILL_REFRESH_MS - 1));
    let start = 5_000 + gnss::STILL_REFRESH_MS;
    assert!(duty.on(false, &rx, start), "an hour after the fix");
    // The refresh holds until its fix has settled.
    feed(&mut rx, &fixed("063012.000"), start + 20_000);
    assert!(duty.on(false, &rx, start + 25_000));
    assert!(!duty.on(false, &rx, start + 30_000), "settled: off");
    assert!(!duty.moving(), "one refresh fix: not moving");
    // A refresh that finds nothing gives up after two minutes.
    let next = start + 30_000 + gnss::STILL_REFRESH_MS;
    assert!(duty.on(false, &rx, next));
    assert!(duty.on(false, &rx, next + gnss::SEARCH_MS - 1));
    assert!(!duty.on(false, &rx, next + gnss::SEARCH_MS));

    // Moving: two refresh fixes a kilometre apart, then five minutes.
    let mut duty = Duty::new();
    let mut rx = Receiver::new();
    rx.power(true, 0);
    assert!(duty.on(true, &rx, 0));
    let mut t = 0;
    for (time, lat) in [("070000.000", "4346.4000"), ("080000.000", "4346.9400")] {
        t += gnss::STILL_REFRESH_MS;
        assert!(duty.on(false, &rx, t), "due at {t}");
        let bytes = [
            line(&format!(
                "GNGGA,{time},{lat},N,01115.3600,E,1,08,0.9,62.3,M,47.0,M,,"
            )),
            line(&format!(
                "GNRMC,{time},A,{lat},N,01115.3600,E,0.0,,091126,,,A,V"
            )),
        ]
        .concat();
        feed(&mut rx, &bytes, t + 1_000);
        t += 1_000 + gnss::SETTLE_MS;
        assert!(!duty.on(false, &rx, t));
    }
    assert!(duty.moving());
    assert_eq!(duty.cadence_ms(), gnss::MOVING_REFRESH_MS);
    assert!(duty.on(false, &rx, t + gnss::MOVING_REFRESH_MS));
}

/// The map's frames while the receiver searches and once it has a fix:
/// the corner's words over the fixture's Florence. Both pinned as
/// goldens; the control is the same view with the receiver off, which
/// draws none of the corner.
#[test]
fn the_maps_corner_frames_for_searching_and_a_fix() {
    let model = Screen::Map.model();
    let drawn = |sky: Sky| {
        let mut frame = Frame::new();
        let mut map = Map::board(FIXTURE, &[]).expect("regions");
        let look = Look::of(&model);
        let mut overlay = look.map;
        overlay.sky = sky;
        Renderer::new(&mut frame)
            .expect("renderer")
            .show(
                &model,
                &Look {
                    map: overlay,
                    ..look
                },
                &mut map,
            )
            .expect("draw");
        frame
    };
    let off = drawn(Sky::Off);
    let searching = drawn(Sky::Searching { in_view: 4 });
    let fix = drawn(Sky::Fix { age_s: 12 });
    support::check("m9-map-sky-searching.png", &searching);
    support::check("m9-map-sky-fix.png", &fix);
    let corner_differs = |a: &Frame, b: &Frame| {
        (0..map::CELL_PX.1 as u32).any(|y| (0..WIDTH / 2).any(|x| a.pixel(x, y) != b.pixel(x, y)))
    };
    assert!(corner_differs(&off, &searching));
    assert!(corner_differs(&searching, &fix));
    // Below the corner's row the three are one picture.
    for y in u32::from(map::CELL_PX.1)..HEIGHT {
        for x in 0..WIDTH {
            assert_eq!(off.pixel(x, y), fix.pixel(x, y), "({x},{y})");
        }
    }
}

/// The own fix the receiver made is the model's (1477 §1, the T1000-E's
/// way, #456): the pin "du" stands at it.
#[test]
fn a_receivers_fix_is_the_models_own() {
    let mut rx = Receiver::new();
    rx.power(true, 0);
    let fix = feed(&mut rx, &fixed("053012.000"), 1_000)[0];
    let mut model: UiModel<Field> = Screen::Map.model();
    update(&mut model, UiEvent::OwnFix(Some(fix)));
    assert_eq!(model.bar.own_fix, Some(fix));
}
