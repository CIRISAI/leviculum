//! The own position set by hand (1371): a desktop has no receiver of its
//! own, so its position sheet takes a fix typed or pasted
//! (`52.5163, 13.3777`, `geo:52.5163,13.3777`) or the map's centre, and
//! from then on shares it as the phone shares its GNSS fix: the same
//! report, the same timed shares ([`super::share`]). The fix is the
//! reader's word, [`Source::Manual`](leviculum_lxmf::lev_position::Source)
//! where it goes into a `lev.position` announce
//! ([`crate::mesh_position::manual`]); a chat's report is telemetry, which
//! carries no source.
//!
//! A shell that takes a fix by hand says so with
//! [`super::compose::Abilities::manual`]; the phone, which reads its own,
//! does not.

use alloc::string::ToString;
use alloc::vec::Vec;

use leviculum_lxmf::telemetry::Location;
use leviculum_ui::toast::ToastKind;

use super::{UiEffect, UiModel};

/// Degrees times 10^6 of `text`, a latitude and a longitude in decimal
/// degrees, split by a comma, a semicolon or blanks, optionally behind
/// `geo:`; none when it is not that or lies off the globe. Read exactly,
/// digit by digit, without a float: the seventh decimal rounds.
pub fn parse(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    let text = text
        .strip_prefix("geo:")
        .or_else(|| text.strip_prefix("GEO:"))
        .unwrap_or(text);
    // A geo URI's parameters (`;u=35`, `?z=12`) are not the fix.
    let text = text.split(['?', ';']).next().unwrap_or_default();
    let mut parts = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty());
    let lat = degrees_e6(parts.next()?)?;
    let lon = degrees_e6(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    let on_globe =
        (-90_000_000..=90_000_000).contains(&lat) && (-180_000_000..=180_000_000).contains(&lon);
    on_globe.then_some((lat, lon))
}

/// One decimal number of degrees, times 10^6.
fn degrees_e6(word: &str) -> Option<i32> {
    let (negative, digits) = match word.as_bytes().first()? {
        b'-' => (true, &word[1..]),
        b'+' => (false, &word[1..]),
        _ => (false, word),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(fraction) || whole.len() > 3 {
        return None;
    }
    let mut value: i64 = whole.bytes().fold(0, |v, b| v * 10 + i64::from(b - b'0'));
    let mut places = fraction.bytes();
    for _ in 0..6 {
        let digit = places.next().map_or(0, |b| i64::from(b - b'0'));
        value = value * 10 + digit;
    }
    if places.next().is_some_and(|b| b >= b'5') {
        value += 1;
    }
    let value = if negative { -value } else { value };
    i32::try_from(value).ok()
}

/// A fix at degrees times 10^6, taken at `now` (unix seconds): only the
/// place, no height, speed, bearing or accuracy.
pub fn location(lat_e6: i32, lon_e6: i32, now: i64) -> Location {
    Location {
        latitude_e6: lat_e6,
        longitude_e6: lon_e6,
        altitude_e2: 0,
        speed_e2: 0,
        bearing_e2: 0,
        accuracy_e2: 0,
        last_update: now,
    }
}

/// The map's centre at a world point.
pub fn of_world(at: leviculum_map::mercator::World, now: i64) -> Location {
    let degrees = leviculum_map::mercator::unproject(at);
    location(degrees.lat_e7 / 10, degrees.lon_e7 / 10, now)
}

impl<B> UiModel<B> {
    /// The fix set by hand, now: the position sheet's shares take it, and
    /// a running share sends it when due.
    pub(super) fn fix_by_hand(&mut self, location: Location) -> Vec<UiEffect> {
        let now = self.now.map_or(location.last_update, |now| now.unix_secs);
        self.bar.own_fix = Some(Location {
            last_update: now,
            ..location
        });
        self.bar.by_hand = true;
        self.shares_due()
    }

    /// [`super::UiEvent::EnterPosition`]: `text` read as a fix, or the
    /// toast that says what one looks like.
    pub(super) fn enter_position(&mut self, text: &str) -> Vec<UiEffect> {
        match parse(text) {
            Some((lat, lon)) => {
                let now = self.now.map_or(0, |now| now.unix_secs);
                self.toast(
                    ToastKind::Info,
                    alloc::format!("your position is {}, {}", e6_words(lat), e6_words(lon)),
                );
                self.fix_by_hand(location(lat, lon, now))
            }
            None => {
                self.toast(ToastKind::Error, NOT_A_POSITION.to_string());
                Vec::new()
            }
        }
    }

    /// The map's centre, when a map was open: the shell's, else the
    /// terminal's own map.
    pub fn map_centre(&self) -> Option<Location> {
        let now = self.now.map_or(0, |now| now.unix_secs);
        self.bar
            .map_centre
            .or_else(|| self.map.as_ref().map(|map| of_world(map.centre, now)))
    }

    /// [`super::UiEvent::FixAtMapCentre`]: the map's centre is the fix.
    pub(super) fn fix_at_map_centre(&mut self) -> Vec<UiEffect> {
        match self.map_centre() {
            Some(centre) => self.fix_by_hand(centre),
            None => {
                self.toast(ToastKind::Info, NO_MAP_CENTRE.to_string());
                Vec::new()
            }
        }
    }
}

/// What a fix typed by hand looks like.
pub const NOT_A_POSITION: &str =
    "not a position: latitude and longitude in degrees, like 52.5163, 13.3777";

/// The map's centre asked for with no map open.
pub const NO_MAP_CENTRE: &str = "open the map first: its centre is the position";

/// Degrees times 10^6 as words, six places.
fn e6_words(e6: i32) -> alloc::string::String {
    let sign = if e6 < 0 { "-" } else { "" };
    let abs = e6.unsigned_abs();
    alloc::format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::compose::{position_rows, Abilities, SheetRow};
    use crate::ui::share::Span;
    use crate::ui::tests::Field;
    use crate::ui::{update, UiEvent};
    use alloc::string::String;
    use leviculum_lxmf::constants::FIELD_TELEMETRY;

    const PEER: [u8; 16] = [0xab; 16];

    fn desktop() -> UiModel<Field> {
        let mut model = UiModel::new(&[PEER]);
        model.bar.abilities = Abilities::desktop();
        update(&mut model, UiEvent::Open { peer: PEER });
        update(
            &mut model,
            UiEvent::Clock(crate::when::WallTime {
                unix_secs: 1_800_000_000,
                offset_secs: 0,
            }),
        );
        model
    }

    /// The forms a reader types or pastes, and the ones that are none.
    #[test]
    fn a_typed_position_is_read_exactly_and_nonsense_is_refused() {
        assert_eq!(parse("52.5163, 13.3777"), Some((52_516_300, 13_377_700)));
        assert_eq!(parse("52.5163 13.3777"), Some((52_516_300, 13_377_700)));
        assert_eq!(
            parse(" -33.8568,151.2153 "),
            Some((-33_856_800, 151_215_300))
        );
        assert_eq!(
            parse("geo:48.1371,11.5754;u=35"),
            Some((48_137_100, 11_575_400))
        );
        assert_eq!(parse("52,13"), Some((52_000_000, 13_000_000)));
        assert_eq!(
            parse("0.12345675, -0.0000004"),
            Some((123_457, 0)),
            "the seventh place rounds"
        );
        assert_eq!(parse("90, 180"), Some((90_000_000, 180_000_000)));
        for none in [
            "",
            "52.5",
            "91, 0",
            "0, 181",
            "Berlin",
            "52.5, 13.4, 7",
            "1e2, 3",
            "--1, 2",
            ".",
            "1000, 0",
        ] {
            assert_eq!(parse(none), None, "{none:?}");
        }
    }

    /// 1371: the desktop has no fix, so its sheet offers the entry; a
    /// typed position makes the shares possible and "now" sends it as the
    /// phone's report. Control: the phone's sheet without a fix still
    /// asks to turn location on, and offers no entry.
    #[test]
    fn a_position_typed_on_the_desktop_is_shared_as_the_phones_fix() {
        let mut model = desktop();
        assert_eq!(
            position_rows(None, model.bar.abilities, false),
            [SheetRow::EnterPosition]
        );
        assert_eq!(
            position_rows(None, Abilities::phone(true), true),
            [SheetRow::LocationOn],
            "control: the phone reads its own"
        );
        let refused = update(
            &mut model,
            UiEvent::EnterPosition {
                text: String::from("somewhere"),
            },
        );
        assert!(refused.is_empty());
        assert_eq!(model.bar.own_fix, None);

        update(
            &mut model,
            UiEvent::EnterPosition {
                text: String::from("52.5163, 13.3777"),
            },
        );
        let fix = model.bar.own_fix.expect("the typed fix");
        assert_eq!(
            (fix.latitude_e6, fix.longitude_e6),
            (52_516_300, 13_377_700)
        );
        assert_eq!(fix.last_update, 1_800_000_000);
        assert!(model.bar.by_hand);
        let rows = position_rows(Some(&fix), model.bar.abilities, false);
        assert_eq!(rows[0], SheetRow::ShareNow);
        assert!(rows.contains(&SheetRow::ShareFor(Span::Hour1)));
        assert_eq!(rows.last(), Some(&SheetRow::EnterPosition), "it can change");

        let sent = update(&mut model, UiEvent::ShareFix { peer: PEER });
        let fields = sent
            .iter()
            .find_map(|e| match e {
                UiEffect::Send { fields, .. } => Some(fields.clone()),
                _ => None,
            })
            .expect("a report went");
        assert!(fields.iter().any(|(key, _)| *key == FIELD_TELEMETRY));
    }

    /// The map's centre is the fix when a map was open, and is offered
    /// only then; without one the sheet has no such row and the event
    /// says so.
    #[test]
    fn the_maps_centre_is_the_fix_only_when_a_map_was_open() {
        let mut model = desktop();
        assert_eq!(model.map_centre(), None);
        assert!(update(&mut model, UiEvent::FixAtMapCentre).is_empty());
        assert_eq!(model.bar.own_fix, None, "control: no map, no fix");

        let centre = location(48_137_100, 11_575_400, 0);
        update(&mut model, UiEvent::MapCentre(Some(centre)));
        assert_eq!(
            position_rows(None, model.bar.abilities, model.map_centre().is_some()),
            [SheetRow::EnterPosition, SheetRow::MapCentre]
        );
        update(&mut model, UiEvent::FixAtMapCentre);
        let fix = model.bar.own_fix.expect("the centre");
        assert_eq!(
            (fix.latitude_e6, fix.longitude_e6),
            (48_137_100, 11_575_400)
        );
    }

    /// The world point of a place comes back as that place, to a metre.
    #[test]
    fn a_world_point_is_its_place_again() {
        let at = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_e6(
            52_516_300, 13_377_700,
        ));
        let back = of_world(at, 5);
        assert!((back.latitude_e6 - 52_516_300).abs() <= 10);
        assert!((back.longitude_e6 - 13_377_700).abs() <= 10);
        assert_eq!(back.last_update, 5);
    }
}
