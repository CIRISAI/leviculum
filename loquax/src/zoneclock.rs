//! The time zone view's clocks (1684, `docs/src/concepts/loquax-map.md`):
//! one control in the map's menu, and the world shows its zones with the
//! current time written into each.
//!
//! The outlines and the label anchors are the map core's
//! ([`leviculum_map::timezones`]); the clock rules come from the system's
//! own tzdata, `/usr/share/zoneinfo` on Linux and the packed `tzdata` file
//! on Android ([`timezones::system_zone_bytes`]), read once a zone and
//! kept. A system without a zone falls back to the board's table, the
//! rules the M9 carries ([`timezones::board_offset`]).

use std::collections::HashMap;
use std::string::String;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::vec::Vec;

use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::labels::ViewLabel;
use leviculum_map::timezones::{self, tzif};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::style::MapTheme;

/// The system's zone files read so far, `None` for one it lacks.
fn system_zones() -> &'static Mutex<HashMap<String, Option<Vec<u8>>>> {
    static ZONES: OnceLock<Mutex<HashMap<String, Option<Vec<u8>>>>> = OnceLock::new();
    ZONES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Zone `name`'s UTC offset in seconds at `unix`: by the system's tzdata,
/// else by the board's table; `None` for a zone neither knows.
pub fn offset(name: &str, unix: i64) -> Option<i32> {
    let mut zones = system_zones()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let bytes = zones
        .entry(String::from(name))
        .or_insert_with(|| timezones::system_zone_bytes(name));
    if let Some(off) = bytes
        .as_deref()
        .and_then(|b| tzif::offset_at(b, unix))
        .map(|o| o.utc_offset)
    {
        return Some(off);
    }
    drop(zones);
    timezones::board_offset(name, unix)
}

/// Zone `name`'s wall clock at `unix` as `HH:MM`.
pub fn clock(name: &str, unix: i64) -> Option<String> {
    let local = unix.checked_add(i64::from(offset(name, unix)?))?;
    Some(hh_mm(local))
}

/// Now in seconds since the epoch, on the minute: what the clocks show,
/// and the same for a whole minute, so a frontend that asks for a frame
/// twice in it finds the one on screen still current.
pub fn unix_minute() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    now - now.rem_euclid(60)
}

/// `local` seconds as the clock on the wall shows them, `HH:MM`.
pub fn hh_mm(local: i64) -> String {
    let minute = local.div_euclid(60).rem_euclid(24 * 60);
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// The zones' labels `view` shows, each one's text its time at `unix`;
/// a zone no rules know is left out.
pub fn labels(view: &Viewport, unix: i64) -> Vec<ViewLabel> {
    let mut out = Vec::new();
    timezones::labels(view, &mut out);
    clocked(out, unix)
}

/// [`labels`] over the blended world view (1710): each zone's clock
/// anchored by the blend, so it sits on its zone's blended ground.
pub fn labels_view(blend: &leviculum_map::blend::Blend, unix: i64) -> Vec<ViewLabel> {
    let mut out = Vec::new();
    timezones::labels_view(blend, &mut out);
    clocked(out, unix)
}

/// The zones' labels with each one's text its wall clock at `unix`; a
/// zone no rules know is left out.
fn clocked(mut out: Vec<ViewLabel>, unix: i64) -> Vec<ViewLabel> {
    out.retain_mut(|label| match clock(&label.local, unix) {
        Some(time) => {
            label.text = time;
            true
        }
        None => false,
    });
    out
}

/// The colour the zones' borders are drawn in: a violet no other line of
/// the map has, so a zone's edge is not read as a country's.
pub fn line_rgb(theme: MapTheme) -> [u8; 3] {
    match theme {
        MapTheme::Dark => [0xc3, 0x9b, 0xd3],
        _ => [0x8e, 0x44, 0xad],
    }
}

/// [`line_rgb`] as the filler's colour.
pub fn line_colour(theme: MapTheme) -> Rgb565 {
    let [r, g, b] = line_rgb(theme);
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};

    /// 1684: a zone's clock is its offset from UTC, by the system's rules
    /// where they are, the board's where not; a zone nobody knows has none.
    #[test]
    fn a_zone_shows_its_wall_clock() {
        // 2026-07-01T12:00:00Z.
        let noon = 1_782_907_200;
        assert_eq!(clock("Europe/Paris", noon).as_deref(), Some("14:00"));
        assert_eq!(clock("Asia/Kathmandu", noon).as_deref(), Some("17:45"));
        assert_eq!(clock("America/New_York", noon).as_deref(), Some("08:00"));
        assert_eq!(clock("Nowhere/Nothing", noon), None);
        assert_eq!(hh_mm(-60), "23:59");
    }

    /// The world view at z2 writes a clock into its zones, and every label
    /// is a time, never a zone's name.
    #[test]
    fn the_world_view_writes_the_time_into_its_zones() {
        let view = Viewport::new(project(LatLon::from_degrees(20.0, 10.0)), 2, 1024, 768);
        let noon = 1_782_907_200;
        let labels = labels(&view, noon);
        assert!(labels.len() > 10, "{}", labels.len());
        for l in &labels {
            assert_eq!(l.text.len(), 5, "{:?}", l.text);
            assert_eq!(&l.text[2..3], ":");
        }
        assert!(labels
            .iter()
            .any(|l| l.local == "Europe/Paris" && l.text == "14:00"));
    }
}
