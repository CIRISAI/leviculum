//! A map pin's card and the map's locations list (1430): what a window
//! shows of the peer a click on its pin selected, at the map's bottom, and
//! the compact list of everyone with a position it opens over the map on
//! demand. The phone's card (`MapPane` in the Android app) has the same
//! fields from the facade's friend rows; here they are built once from
//! the model, so the window and any later pixel screen word them alike.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_map::mercator::{unproject, World};

use super::chat::list_row;
use super::map::MapPin;
use super::radar;
use super::scene::{self, Avatar, Ring};
use super::UiModel;

/// One peer with a position, as its card and the locations list say it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinCard {
    pub peer: [u8; 16],
    /// The display rule's name: the name alone, its hash tail only where
    /// two peers share it ([`crate::names::Names::label`]).
    pub name: String,
    pub avatar: Avatar,
    /// The card's status line: the chat row's liveness words ("live ·
    /// every 1 min · Now", "last position 3 h ago"), and the distance
    /// after them when the own position is known.
    pub status: String,
    /// How old the fix is in the catalogue's words, the list's column.
    pub age: String,
    /// The distance from the own position, when there is one.
    pub distance: Option<String>,
    /// The fix's time, unix seconds: the list is sorted by it.
    pub fix_at: i64,
    /// Where the pin is.
    pub at: World,
    /// Our own position: its card has no actions.
    pub own: bool,
    /// A node the mesh announced that is no contact (1302).
    pub nearby: bool,
}

impl PinCard {
    /// Whether the card offers Chat and Share mine: a peer, not ourselves
    /// and not a node nearby that is no contact.
    pub fn actions(&self) -> bool {
        !self.own && !self.nearby
    }
}

/// Every pin of `model`'s map as a card, in the pins' order.
pub fn cards<B>(model: &UiModel<B>) -> Vec<PinCard> {
    let pins = model.map_pins();
    let own = pins.iter().find(|p| p.own).map(|p| p.at);
    pins.iter().map(|pin| card(model, pin, own)).collect()
}

/// The locations list (1430): every card but our own, the newest fix
/// first.
pub fn locations(cards: &[PinCard]) -> Vec<&PinCard> {
    let mut list: Vec<&PinCard> = cards.iter().filter(|c| !c.own).collect();
    list.sort_by(|a, b| b.fix_at.cmp(&a.fix_at).then(a.name.cmp(&b.name)));
    list
}

fn card<B>(model: &UiModel<B>, pin: &MapPin, own: Option<World>) -> PinCard {
    let lang = model.lang;
    let now = model.now.map(|now| now.unix_secs);
    let liveness = model.liveness(&pin.peer);
    let age = pin.age.clone().unwrap_or_default();
    let live = match liveness.last_fix_at {
        Some(_) => scene::row_live(&liveness, now).resolve(lang),
        None => age.clone(),
    };
    let distance = own
        .filter(|_| !pin.own)
        .map(|own| radar::distance(radar::metres(own, pin.at)));
    let status = match (&distance, live.is_empty()) {
        (Some(d), false) => format!("{live} · {d}"),
        (Some(d), true) => d.clone(),
        (None, _) => live,
    };
    let row = list_row(model, &pin.peer);
    let avatar = Avatar::of(
        row.initials,
        &pin.peer,
        row.icon.as_ref(),
        match pin.nearby {
            true => None,
            false => Ring::of(liveness.state),
        },
    );
    PinCard {
        peer: pin.peer,
        name: pin.label.clone(),
        avatar,
        status,
        age,
        distance,
        fix_at: pin.fix_at.unwrap_or_default(),
        at: pin.at,
        own: pin.own,
        nearby: pin.nearby,
    }
}

/// Directions to `to` on OpenStreetMap's site (1430), from `from` when
/// the own position is known: the window's Navigate, which a desktop's
/// browser opens; a phone hands a `geo:` link to its navigation app
/// instead.
pub fn directions(to: World, from: Option<World>) -> String {
    let at = |w: World| {
        let p = unproject(w);
        format!("{},{}", degrees(p.lat_e7), degrees(p.lon_e7))
    };
    let from = from.map(at).unwrap_or_default();
    format!(
        "https://www.openstreetmap.org/directions?from={from}&to={}",
        at(to)
    )
}

/// Degrees times ten million as a decimal of six places, `43.775630`.
fn degrees(e7: i32) -> String {
    let micro = (i64::from(e7) + i64::from(e7.signum()) * 5) / 10;
    let sign = if micro < 0 { "-" } else { "" };
    let micro = micro.unsigned_abs();
    format!("{sign}{}.{:06}", micro / 1_000_000, micro % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};

    #[test]
    fn directions_name_the_goal_and_the_start_when_known() {
        let to = project(LatLon::from_e6(43_775_630, 11_260_220));
        let from = project(LatLon::from_e6(-33_868_800, 151_209_300));
        let url = directions(to, None);
        assert!(url.starts_with("https://www.openstreetmap.org/directions?from=&to=43.7756"));
        assert!(url.contains(",11.2602"), "{url}");
        let both = directions(to, Some(from));
        assert!(both.contains("from=-33.8688"), "{both}");
        assert!(both.contains(",151.2093"), "{both}");
        assert_eq!(degrees(-5), "-0.000001");
        assert_eq!(degrees(437_756_300), "43.775630");
    }
}
