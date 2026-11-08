//! Which region's radio preset a board should come up on, estimated from the
//! host it is flashed on (#476).
//!
//! The zero-config flash asks nothing, so the radio step needs an answer the
//! person never typed. The rule, in order:
//!
//! 1. **The time zone**, mapped to a country by the system's own `zone.tab`
//!    (`/usr/share/zoneinfo/zone.tab`, one ISO 3166 code per zone). A zone
//!    says where the host is; it is set by the installer from the place the
//!    person picked, and unlike the language it is rarely left at a default
//!    from somewhere else. `$TZ` first, then `/etc/timezone`, then the
//!    target of the `/etc/localtime` link.
//! 2. **The locale's territory**, `de_DE` in `de_DE.UTF-8`: `$LC_ALL`, then
//!    `$LC_MEASUREMENT`, then `$LANG`. Second, because `en_US` is the
//!    language many people everywhere run their machines in.
//! 3. **Neither names a country with a preset**: the shipped profile, eu868,
//!    the one the firmware compiles in (`radio::EU868`), said as such.
//!
//! A country maps to a preset only where the preset's frequency is one the
//! country's short-range-device rules allow: CEPT Europe for eu868, the US,
//! Canada and Mexico for us915, Australia and New Zealand for au915. Every
//! other country gets no estimate rather than a neighbour's band, because a
//! wrong band is a board transmitting where it must not.
//!
//! What a board already carries outranks all of this: the flow keeps a
//! profile the board reports that is not the shipped one (`flow::set_radio`).

use std::path::{Path, PathBuf};

use crate::radio::{self, PresetDef};

/// Where `zone.tab` lives on every Linux distribution with tzdata.
pub const ZONE_TAB: &str = "/usr/share/zoneinfo/zone.tab";

/// Countries whose short-range-device band holds the eu868 preset's
/// 869.463 MHz: the EU and EEA, and the other CEPT members that adopted ERC
/// Recommendation 70-03 annex 1 band 54 (869.4 to 869.65 MHz).
const EU868_COUNTRIES: &[&str] = &[
    "AD", "AL", "AT", "BA", "BE", "BG", "CH", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FO", "FR",
    "GB", "GG", "GI", "GR", "HR", "HU", "IE", "IM", "IS", "IT", "JE", "LI", "LT", "LU", "LV", "MC",
    "MD", "ME", "MK", "MT", "NL", "NO", "PL", "PT", "RO", "RS", "SE", "SI", "SK", "SM", "TR", "UA",
    "VA",
];

/// Countries whose 902-928 MHz ISM band holds the us915 preset.
const US915_COUNTRIES: &[&str] = &["CA", "MX", "US"];

/// Countries whose 915-928 MHz band holds the au915 preset.
const AU915_COUNTRIES: &[&str] = &["AU", "NZ"];

/// What the host says about where it is. Read once by [`Place::from_host`];
/// a test builds one by hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    /// The time zone name, `Europe/Berlin`.
    pub zone: Option<String>,
    /// The contents of `zone.tab`, when it could be read.
    pub zone_tab: Option<String>,
    /// The locale string the territory is read from, `de_DE.UTF-8`.
    pub locale: Option<String>,
}

impl Place {
    /// The running host's zone, `zone.tab` and locale.
    pub fn from_host() -> Self {
        Self::read(
            |name| std::env::var(name).ok(),
            Path::new("/etc/timezone"),
            Path::new("/etc/localtime"),
            Path::new(ZONE_TAB),
        )
    }

    /// [`Place::from_host`] over the given environment and files.
    pub fn read(
        env: impl Fn(&str) -> Option<String>,
        timezone_file: &Path,
        localtime_link: &Path,
        zone_tab: &Path,
    ) -> Self {
        let zone = env("TZ")
            .map(|tz| tz.trim_start_matches(':').trim().to_string())
            .filter(|tz| !tz.is_empty())
            .or_else(|| {
                std::fs::read_to_string(timezone_file)
                    .ok()
                    .map(|text| text.trim().to_string())
                    .filter(|tz| !tz.is_empty())
            })
            .or_else(|| zone_from_link(localtime_link));
        let locale = ["LC_ALL", "LC_MEASUREMENT", "LANG"]
            .into_iter()
            .filter_map(&env)
            .find(|value| !value.trim().is_empty());
        Self {
            zone,
            zone_tab: std::fs::read_to_string(zone_tab).ok(),
            locale,
        }
    }
}

/// `Europe/Berlin` from `/usr/share/zoneinfo/Europe/Berlin`, the target of
/// `/etc/localtime`.
fn zone_from_link(link: &Path) -> Option<String> {
    let target: PathBuf = std::fs::read_link(link).ok()?;
    let text = target.to_str()?;
    let (_, zone) = text.split_once("zoneinfo/")?;
    let zone = zone
        .trim_start_matches("posix/")
        .trim_start_matches("right/");
    (!zone.is_empty()).then(|| zone.to_string())
}

/// The country `zone.tab` gives a zone, upper case.
fn country_of_zone(zone_tab: &str, zone: &str) -> Option<String> {
    zone_tab
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut fields = line.split('\t');
            let country = fields.next()?;
            let _coordinates = fields.next()?;
            (fields.next()? == zone).then(|| country.trim().to_ascii_uppercase())
        })
}

/// `DE` from `de_DE.UTF-8` or `de_DE@euro`; nothing from `C` or `POSIX`.
fn territory_of_locale(locale: &str) -> Option<String> {
    let base = locale.split(['.', '@']).next()?;
    let (_, territory) = base.split_once('_')?;
    (territory.len() == 2 && territory.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| territory.to_ascii_uppercase())
}

fn preset_for_country(country: &str) -> Option<&'static PresetDef> {
    let name = if EU868_COUNTRIES.contains(&country) {
        "eu868"
    } else if US915_COUNTRIES.contains(&country) {
        "us915"
    } else if AU915_COUNTRIES.contains(&country) {
        "au915"
    } else {
        return None;
    };
    radio::preset(name).ok()
}

/// The preset the zero-config flash would send, and the sentence that says
/// where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Estimate {
    pub preset: &'static PresetDef,
    /// Why this preset: "time zone Europe/Berlin (DE)" and the like.
    pub why: String,
}

impl Default for Estimate {
    /// The shipped profile with nothing read: what a run that never looked
    /// at its host sends.
    fn default() -> Self {
        Self {
            preset: radio::EU868_PRESET,
            why: "no region was estimated, so the shipped default".to_string(),
        }
    }
}

/// The rule in the module documentation, over what [`Place`] read.
pub fn estimate(place: &Place) -> Estimate {
    let mut unmatched: Vec<String> = Vec::new();
    if let Some(zone) = &place.zone {
        match place
            .zone_tab
            .as_deref()
            .and_then(|tab| country_of_zone(tab, zone))
        {
            Some(country) => match preset_for_country(&country) {
                Some(preset) => {
                    return Estimate {
                        preset,
                        why: format!("time zone {zone} ({country})"),
                    }
                }
                None => unmatched.push(format!("time zone {zone} ({country})")),
            },
            None => unmatched.push(format!(
                "time zone {zone}, a country zone.tab does not name"
            )),
        }
    }
    if let Some(locale) = &place.locale {
        match territory_of_locale(locale) {
            Some(country) => match preset_for_country(&country) {
                Some(preset) => {
                    return Estimate {
                        preset,
                        why: format!("locale {locale} ({country})"),
                    }
                }
                None => unmatched.push(format!("locale {locale} ({country})")),
            },
            None => unmatched.push(format!("locale {locale}, which names no country")),
        }
    }
    let why = if unmatched.is_empty() {
        "neither a time zone nor a locale is set, so the shipped default".to_string()
    } else {
        format!(
            "{} has no preset here, so the shipped default",
            unmatched.join(" and ")
        )
    };
    Estimate {
        preset: radio::EU868_PRESET,
        why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    const TAB: &str = "# comment\nDE\t+5230+01322\tEurope/Berlin\tmost of Germany\n\
                       US\t+404251-0740023\tAmerica/New_York\tEastern (most areas)\n\
                       AU\t-3352+15113\tAustralia/Sydney\tNew South Wales (most areas)\n\
                       BR\t-2332-04637\tAmerica/Sao_Paulo\tBrazil (southeast)\n";

    fn place(zone: Option<&str>, locale: Option<&str>) -> Place {
        Place {
            zone: zone.map(str::to_string),
            zone_tab: Some(TAB.to_string()),
            locale: locale.map(str::to_string),
        }
    }

    #[test]
    fn the_time_zone_decides_before_the_language() {
        // en_US is the language; Berlin is the place, and the band is the
        // place's.
        let got = estimate(&place(Some("Europe/Berlin"), Some("en_US.UTF-8")));
        assert_eq!(got.preset.name, "eu868");
        assert_eq!(got.why, "time zone Europe/Berlin (DE)");
        assert_eq!(
            estimate(&place(Some("America/New_York"), Some("de_DE.UTF-8")))
                .preset
                .name,
            "us915"
        );
        assert_eq!(
            estimate(&place(Some("Australia/Sydney"), None)).preset.name,
            "au915"
        );
    }

    #[test]
    fn without_a_zone_the_locale_territory_decides() {
        let got = estimate(&place(None, Some("en_NZ.UTF-8")));
        assert_eq!(got.preset.name, "au915");
        assert_eq!(got.why, "locale en_NZ.UTF-8 (NZ)");
        assert_eq!(
            estimate(&place(None, Some("fr_CA@euro"))).preset.name,
            "us915"
        );
    }

    #[test]
    fn a_country_without_a_preset_falls_to_the_shipped_default_and_says_so() {
        // Brazil's band does not hold 914.875 MHz; a neighbour's preset would
        // be a board transmitting where it must not.
        let got = estimate(&place(Some("America/Sao_Paulo"), Some("C.UTF-8")));
        assert_eq!(got.preset.name, "eu868");
        assert!(
            got.why.contains("time zone America/Sao_Paulo (BR)"),
            "{}",
            got.why
        );
        assert!(
            got.why.contains("locale C.UTF-8, which names no country"),
            "{}",
            got.why
        );
        assert!(got.why.ends_with("so the shipped default"), "{}", got.why);
        let none = estimate(&Place::default());
        assert_eq!(none.preset.name, "eu868");
        assert!(
            none.why.contains("neither a time zone nor a locale"),
            "{}",
            none.why
        );
    }

    #[test]
    fn the_host_is_read_from_tz_then_the_timezone_file_then_the_localtime_link() {
        let dir = TempDir::new().unwrap();
        let tab = dir.path().join("zone.tab");
        std::fs::write(&tab, TAB).unwrap();
        let link = dir.path().join("localtime");
        symlink("/usr/share/zoneinfo/Australia/Sydney", &link).unwrap();
        let file = dir.path().join("timezone");
        let missing = dir.path().join("nothing");

        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        let read = Place::read(env(&[]), &missing, &link, &tab);
        assert_eq!(read.zone.as_deref(), Some("Australia/Sydney"));
        assert_eq!(read.zone_tab.as_deref(), Some(TAB));

        std::fs::write(&file, "America/New_York\n").unwrap();
        let read = Place::read(env(&[("LANG", "de_DE.UTF-8")]), &file, &link, &tab);
        assert_eq!(read.zone.as_deref(), Some("America/New_York"));
        assert_eq!(read.locale.as_deref(), Some("de_DE.UTF-8"));

        let read = Place::read(
            env(&[
                ("TZ", ":Europe/Berlin"),
                ("LC_ALL", ""),
                ("LC_MEASUREMENT", "en_AU.UTF-8"),
                ("LANG", "de_DE.UTF-8"),
            ]),
            &file,
            &link,
            &tab,
        );
        assert_eq!(read.zone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(read.locale.as_deref(), Some("en_AU.UTF-8"));
    }

    #[test]
    fn every_country_list_names_a_preset_the_table_carries() {
        for country in EU868_COUNTRIES
            .iter()
            .chain(US915_COUNTRIES)
            .chain(AU915_COUNTRIES)
        {
            let preset = preset_for_country(country).unwrap();
            assert!(preset.unavailable.is_none(), "{country}");
        }
    }
}
