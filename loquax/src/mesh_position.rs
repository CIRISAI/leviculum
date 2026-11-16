//! Positions the mesh announces (1302): a node's `lev.position`
//! destination, announced beside its delivery destination, carries its
//! fix in the app data, so the map shows every node in range with no
//! mailbox and no message (`docs/drafts/mailbox-for-friends-study-2026-11.md`,
//! 4.3).
//!
//! The destination is SINGLE, of the node's identity, so the identity
//! hash the announce proves also names the node's delivery address
//! ([`delivery_of`]): the core merges an announced fix with the LXMF
//! telemetry positions of the same peer ([`crate::ui::UiEvent::MeshPosition`]).
//!
//! The app data is the board side's codec, [`leviculum_lxmf::lev_position`]
//! (order 1301): a msgpack map, plain or sealed to the owner's identity.
//! [`read`] shows what is plain and what is sealed to us, and nothing
//! sealed to somebody else: a board that shares with everyone nearby is
//! on the map, a stranger's board that shares only with its owner is not.
//! Privacy over visibility.

use alloc::string::String;
use leviculum_core::identity::Identity;
use leviculum_core::Destination;
use leviculum_lxmf::lev_position::{self, AppData, Position, Source};
use leviculum_lxmf::telemetry::Location;

pub use leviculum_lxmf::lev_position::{name_hash, APP_NAME, ASPECT};

/// Whether an announce's name hash is `lev.position`'s.
pub fn is_position(heard: &[u8]) -> bool {
    heard == name_hash()
}

/// The delivery address (`lxmf.delivery`) of the identity whose hash is
/// `identity`: the peer the core keys positions by.
pub fn delivery_of(identity: &[u8; 16]) -> [u8; 16] {
    let name = Destination::compute_name_hash(
        leviculum_lxmf::node::APP_NAME,
        &[leviculum_lxmf::node::DELIVERY_ASPECT],
    );
    *Destination::compute_destination_hash(&name, identity).as_bytes()
}

/// The position in an announce's app data: a plain one always, a sealed
/// one only when `own` (an identity with its private key, the home
/// identity) is the one it was sealed to. A client of a home node holds
/// no private key and passes `None`, so it reads the plain ones.
pub fn read(app_data: &[u8], own: Option<&Identity>) -> Option<Position> {
    match own {
        Some(own) => lev_position::open(app_data, own),
        None => match AppData::parse(app_data)? {
            AppData::Plain(position) => Some(position),
            AppData::Sealed(_) => None,
        },
    }
}

/// `loquax announce --position`: a fixed position someone set, plain, so
/// every node in range reads it (the device walk's node nearby), under
/// `name` when there is one.
pub fn manual(location: &Location, name: Option<String>) -> Position {
    Position {
        lat_e7: location.latitude_e6.saturating_mul(10),
        lon_e7: location.longitude_e6.saturating_mul(10),
        alt_m: None,
        speed_cm_s: None,
        heading_e2: None,
        fix_age_s: 0,
        source: Some(Source::Manual),
        name: name.filter(|name| !name.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

    fn florence() -> Location {
        Location {
            latitude_e6: 43_773_100,
            longitude_e6: 11_256_000,
            altitude_e2: 0,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 0,
            last_update: 1_793_500_000,
        }
    }

    /// 1313: a manual position is plain, so anyone reads it, and it is
    /// the fix it was made from, heard now.
    #[test]
    fn a_manual_position_is_plain_and_reads_back_as_its_fix() {
        let position = manual(&florence(), Some("Walk Nearby A".into()));
        assert_eq!(position.source, Some(Source::Manual));
        let data = position.encode();
        assert!(matches!(AppData::parse(&data), Some(AppData::Plain(_))));
        let read = read(&data, None).expect("plain reads without a key");
        assert_eq!(read.location(1_793_500_000), florence());
        assert_eq!(read.name.as_deref(), Some("Walk Nearby A"));
        assert_eq!(manual(&florence(), Some(String::new())).name, None);
    }

    /// 1313: a position sealed to somebody else is no position, with or
    /// without a key of our own; sealed to us it is ours to read.
    #[test]
    fn a_sealed_position_reads_only_with_the_key_it_was_sealed_to() {
        let home = Identity::generate(&mut OsRng);
        let stranger = Identity::generate(&mut OsRng);
        let position = manual(&florence(), None);
        let theirs = position.seal(&stranger, &mut OsRng).expect("seal");
        assert!(matches!(AppData::parse(&theirs), Some(AppData::Sealed(_))));
        assert_eq!(read(&theirs, None), None, "a client holds no key");
        assert_eq!(read(&theirs, Some(&home)), None, "not ours");
        let ours = position.seal(&home, &mut OsRng).expect("seal");
        assert_eq!(read(&ours, Some(&home)), Some(position));
    }

    #[test]
    fn the_aspect_is_lev_position_and_the_peer_its_delivery_address() {
        assert_eq!(
            name_hash(),
            Destination::compute_name_hash("lev", &["position"])
        );
        assert!(is_position(&name_hash()));
        assert!(!is_position(&Destination::compute_name_hash(
            "lxmf",
            &["delivery"]
        )));
        let identity = [7u8; 16];
        let lxmf = Destination::compute_name_hash("lxmf", &["delivery"]);
        assert_eq!(
            delivery_of(&identity),
            *Destination::compute_destination_hash(&lxmf, &identity).as_bytes()
        );
    }
}
