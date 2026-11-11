//! Countries for travel (order 1684): the point lookup over Natural Earth's
//! 1:50m countries and the Wikidata facts, against places and values
//! checked by hand.

use leviculum_map::countries::{
    all_facts, countries, country_at, country_in, facts, facts_in, ring_table, Number, Side,
    COUNTRIES_50M, FACTS,
};
use leviculum_map::outline;

fn at(lat: f64, lon: f64) -> Option<String> {
    country_at(lat, lon).map(|c| String::from_utf8_lossy(&c).into_owned())
}

#[test]
fn points_lie_in_their_countries() {
    let cases: &[(&str, f64, f64, Option<&str>)] = &[
        ("Frankfurt", 50.1109, 8.6821, Some("DE")),
        ("Paris", 48.8566, 2.3522, Some("FR")),
        ("London", 51.5074, -0.1278, Some("GB")),
        ("Tokyo", 35.6895, 139.6917, Some("JP")),
        ("New Delhi", 28.6139, 77.2090, Some("IN")),
        ("Alice Springs", -23.6980, 133.8807, Some("AU")),
        ("Denver", 39.7392, -104.9903, Some("US")),
        (
            "Maseru, Lesotho inside South Africa",
            -29.45,
            27.6,
            Some("LS"),
        ),
        ("Lesotho highlands", -29.6, 28.6, Some("LS")),
        ("Bloemfontein", -29.0852, 26.1596, Some("ZA")),
        ("Oslo, Norway's -99", 59.9139, 10.7522, Some("NO")),
        ("Pristina, Kosovo XK", 42.6629, 21.1655, Some("XK")),
        ("Taipei, Taiwan's CN-TW", 25.0330, 121.5654, Some("TW")),
        ("Hargeisa, Somaliland", 9.5600, 44.0650, Some("SO")),
        ("Christmas Island", -10.49, 105.62, Some("CX")),
        ("Lyon", 45.7640, 4.8357, Some("FR")),
        ("open Atlantic", 30.0, -40.0, None),
        ("Pacific", 0.0, -150.0, None),
        ("North Sea", 55.5, 3.0, None),
    ];
    for &(name, lat, lon, want) in cases {
        assert_eq!(at(lat, lon).as_deref(), want, "{name} ({lat}, {lon})");
    }
}

#[test]
fn points_off_the_map_are_none() {
    assert_eq!(country_at(f64::NAN, 8.0), None);
    assert_eq!(country_at(50.0, f64::INFINITY), None);
}

#[test]
fn hand_checked_facts() {
    let numbers = |code: &[u8; 2]| -> Vec<String> {
        facts(*code)
            .map(|f| f.numbers().map(|n| n.digits.to_owned()).collect())
            .unwrap_or_default()
    };
    // (code, numbers that must be there, side, currency)
    let cases: &[(&[u8; 2], &[&str], Side, &str)] = &[
        (b"DE", &["112", "110"], Side::Right, "EUR"),
        (b"GB", &["999", "112"], Side::Left, "GBP"),
        (b"US", &["911"], Side::Right, "USD"),
        (b"JP", &["110", "119"], Side::Left, "JPY"),
        (b"IN", &["112"], Side::Left, "INR"),
        (b"AU", &["000"], Side::Left, "AUD"),
    ];
    for &(code, want, side, currency) in cases {
        let f = facts(*code).expect("a row");
        let got = numbers(code);
        for n in want {
            assert!(got.iter().any(|g| g == n), "{code:?}: {n} in {got:?}");
        }
        assert_eq!(f.driving, Some(side), "{code:?}");
        assert_eq!(f.currency, Some(currency), "{code:?}");
        assert_eq!(f.code, *code);
    }
    assert_eq!(facts(*b"DE").map(|f| f.name), Some("Germany"));
    assert_eq!(facts(*b"GB").and_then(|f| f.main_number()), Some("999"));
    assert_eq!(facts(*b"AU").and_then(|f| f.main_number()), Some("000"));
    assert_eq!(facts(*b"US").and_then(|f| f.main_number()), Some("911"));
    let police = facts(*b"DE")
        .and_then(|f| f.numbers().find(|n| n.police))
        .map(|n| n.digits);
    assert_eq!(police, Some("110"));
    assert_eq!(
        facts(*b"DE").and_then(|f| f.numbers().next()),
        Some(Number {
            digits: "112",
            police: false,
            ambulance: false,
            fire: false
        })
    );
    // France keeps the CFP franc of its Pacific territories second.
    let fr: Vec<_> = facts(*b"FR")
        .map(|f| f.currencies().collect())
        .unwrap_or_default();
    assert_eq!(fr, ["EUR", "XPF"]);
    assert!(facts(*b"ZZ").is_none());
}

#[test]
fn every_country_on_the_map_has_facts() {
    for (code, _) in countries(COUNTRIES_50M) {
        assert!(
            facts(code).is_some(),
            "{:?}",
            String::from_utf8_lossy(&code)
        );
    }
}

#[test]
fn tables_are_pinned() {
    assert_eq!(COUNTRIES_50M.len(), 200_354);
    assert_eq!(FACTS.len(), 8_421);
    assert_eq!(countries(COUNTRIES_50M).count(), 239);
    let rings = ring_table(COUNTRIES_50M);
    assert_eq!(outline::bits(rings), Some(16));
    assert_eq!(outline::rings(rings).count(), 1_631);
    let held: u32 = countries(COUNTRIES_50M).map(|(_, n)| n).sum();
    assert_eq!(held, 1_631);
    assert_eq!(all_facts(FACTS).count(), 255);
    assert!(FACTS.starts_with("LCF1 255\n"));
    // Sorted by code, each once.
    let codes: Vec<_> = all_facts(FACTS).map(|f| f.code).collect();
    assert!(codes.windows(2).all(|w| w[0] < w[1]));
    let codes: Vec<_> = countries(COUNTRIES_50M).map(|(c, _)| c).collect();
    assert!(codes.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn malformed_tables_never_panic() {
    for cut in [0, 3, 4, 5, 6, 100, 1_000, 5_000, COUNTRIES_50M.len() - 1] {
        let t = &COUNTRIES_50M[..cut];
        let _ = country_in(t, 50.1109, 8.6821);
        let _ = countries(t).count();
        let _ = outline::rings(ring_table(t)).count();
    }
    assert_eq!(country_in(b"LCO1\xff\xff\xff\xff\xff", 50.0, 8.0), None);
    assert_eq!(country_in(b"LWO2\x10\x00", 50.0, 8.0), None);
    // Cut in half: Australia's rings are still there, South Africa's not.
    let half = &COUNTRIES_50M[..COUNTRIES_50M.len() / 2];
    assert_eq!(country_in(half, -23.698, 133.8807), Some(*b"AU"));
    assert_eq!(country_in(half, -29.0852, 26.1596), None);
    // A facts table without its magic has no rows; a bad row is skipped.
    assert!(facts_in("DE\tGermany\t112\tR\tEUR\n", *b"DE").is_none());
    let t = "LCF1 3\nDE\tGermany\t112\tX\t\nd\tbad\t\t\t\nGB\tUK\t999\tL\tGBP\textra\n";
    let de = facts_in(t, *b"DE").expect("row");
    assert_eq!((de.driving, de.currency), (None, None));
    assert_eq!(all_facts(t).count(), 1);
    for cut in 0..FACTS.len() {
        if FACTS.is_char_boundary(cut) {
            let _ = all_facts(&FACTS[..cut]).count();
        }
    }
}
